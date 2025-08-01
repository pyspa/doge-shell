use crate::completion::integrated::IntegratedCompletionEngine;
use crate::completion::{self, Completion, MAX_RESULT};
use crate::dirs;
use crate::input::{Input, InputConfig, display_width};
use crate::parser::Rule;
use crate::prompt::Prompt;
use crate::shell::{SHELL_TERMINAL, Shell};
use anyhow::Context as _;
use anyhow::Result;
use crossterm::cursor;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Print, ResetColor};
use crossterm::terminal::{self, Clear, ClearType, enable_raw_mode};
use crossterm::{execute, queue};
use dsh_types::Context;
use futures::{StreamExt, future::FutureExt, select};
use futures_timer::Delay;
use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::tcsetpgrp;
use parking_lot::RwLock;
use std::io::{StdoutLock, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Display error in a user-friendly format without stack traces
fn display_user_error(err: &anyhow::Error) {
    let error_msg = err.to_string();

    // Check if it's a command not found error
    if error_msg.contains("unknown command:") {
        if let Some(cmd_start) = error_msg.find("unknown command: ") {
            let cmd = &error_msg[cmd_start + 17..]; // Skip "unknown command: "
            eprintln!("dsh: {}: command not found", cmd.trim());
        } else {
            eprintln!("dsh: command not found");
        }
    } else if error_msg.contains("Shell terminated by double Ctrl+C")
        || error_msg.contains("Normal exit")
        || error_msg.contains("Exit by")
    {
        // Don't display normal exit messages
        debug!("Shell exiting normally: {}", error_msg);
    } else {
        // For other errors, display the root cause without debug info
        eprintln!("dsh: {error_msg}");
    }
}

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

#[derive(Eq, PartialEq)]
#[allow(dead_code)]
pub enum ShellEvent {
    Input(Event),
    ScreenResized,
}

/// State management for detecting double Ctrl+C press
#[derive(Debug)]
struct CtrlCState {
    first_press_time: Option<Instant>,
    press_count: u8,
}

impl CtrlCState {
    fn new() -> Self {
        Self {
            first_press_time: None,
            press_count: 0,
        }
    }

    /// Handle Ctrl+C press. Returns true if it's the second press
    fn on_ctrl_c_pressed(&mut self) -> bool {
        let now = Instant::now();

        match self.first_press_time {
            None => {
                // First press
                self.first_press_time = Some(now);
                self.press_count = 1;
                false
            }
            Some(first_time) => {
                if now.duration_since(first_time) <= Duration::from_secs(3) {
                    // Second press within 3 seconds
                    self.press_count = 2;
                    true
                } else {
                    // More than 3 seconds passed, treat as first press
                    self.first_press_time = Some(now);
                    self.press_count = 1;
                    false
                }
            }
        }
    }

    /// Reset state
    fn reset(&mut self) {
        self.first_press_time = None;
        self.press_count = 0;
    }
}

pub struct Repl<'a> {
    pub shell: &'a mut Shell,
    input: Input,
    columns: usize,
    lines: usize,
    tmode: Option<Termios>,
    history_search: Option<String>,
    start_completion: bool,
    completion: Completion,
    integrated_completion: IntegratedCompletionEngine,
    prompt: Arc<RwLock<Prompt>>,
    ctrl_c_state: CtrlCState,
    should_exit: bool,
}

impl<'a> Drop for Repl<'a> {
    fn drop(&mut self) {
        self.save_history();
    }
}

impl<'a> Repl<'a> {
    pub fn new(shell: &'a mut Shell) -> Self {
        let current = std::env::current_dir().unwrap_or_else(|e| {
            warn!(
                "Failed to get current directory: {}, using home directory",
                e
            );
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    warn!("Failed to get home directory, using root");
                    std::path::PathBuf::from("/")
                })
        });
        let prompt = Prompt::new(current, "🐕 < ".to_string());

        let prompt = Arc::new(RwLock::new(prompt));
        shell
            .environment
            .write()
            .chpwd_hooks
            .push(Box::new(Arc::clone(&prompt)));
        let input_config = InputConfig::default();

        Repl {
            shell,
            input: Input::new(input_config),
            columns: 0,
            lines: 0,
            tmode: None,
            history_search: None,
            start_completion: false,
            completion: Completion::new(),
            integrated_completion: IntegratedCompletionEngine::new(),
            prompt,
            ctrl_c_state: CtrlCState::new(),
            should_exit: false,
        }
    }

    fn setup(&mut self) {
        let screen_size = terminal::size().unwrap_or_else(|e| {
            warn!("Failed to get terminal size: {}, using default 80x24", e);
            (80, 24)
        });
        self.columns = screen_size.0 as usize;

        // Initialize integrated completion engine
        debug!("Initializing integrated completion engine...");
        if let Err(e) = self.integrated_completion.initialize_command_completion() {
            warn!("Failed to initialize command completion: {}", e);
        } else {
            debug!("Integrated completion engine initialized successfully");
        }
        self.lines = screen_size.1 as usize;
        enable_raw_mode().ok();
    }

    async fn check_background_jobs(&mut self, output: bool) -> Result<()> {
        let jobs = self.shell.check_job_state().await?;
        let exists = !jobs.is_empty();

        if output && exists {
            // Process jobs first without holding stdout lock
            for mut job in jobs {
                if !job.foreground {
                    job.check_background_all_output().await?;
                }
            }

            // Then output results with lock
            let mut out = std::io::stdout().lock();
            for job in &self.shell.wait_jobs {
                if !job.foreground && output {
                    out.write_fmt(format_args!(
                        "\rdsh: job {} '{}' {}\n",
                        job.job_id, job.cmd, job.state
                    ))?;
                }
            }

            // out.write(b"\r\n").ok();
            self.print_prompt(&mut out);
            out.flush()?;
        }
        Ok(())
    }

    fn save_history(&mut self) {
        if let Some(ref mut history) = self.shell.cmd_history {
            if let Ok(mut history) = history.try_lock() {
                if let Err(e) = history.save() {
                    warn!("Failed to save command history: {}", e);
                }
            } else {
                debug!("Command history is locked, skipping save");
            }
        }
        if let Some(ref mut history) = self.shell.path_history {
            if let Ok(mut history) = history.try_lock() {
                if let Err(e) = history.save() {
                    warn!("Failed to save path history: {}", e);
                }
            } else {
                debug!("Path history is locked, skipping save");
            }
        }
    }

    fn move_cursor_input_end(&self, out: &mut StdoutLock<'static>) {
        let prompt_mark = &self.prompt.read().mark;
        let prompt_display_width = display_width(prompt_mark);
        let input_cursor_width = self.input.cursor_display_width();
        let cursor_display_pos = prompt_display_width + input_cursor_width;

        debug!(
            "move_cursor_input_end: prompt_mark='{}', prompt_width={}, input_cursor_width={}, final_pos={}",
            prompt_mark, prompt_display_width, input_cursor_width, cursor_display_pos
        );
        debug!(
            "move_cursor_input_end: input_text='{}', input_cursor_pos={}",
            self.input.as_str(),
            self.input.cursor()
        );

        // Ensure we don't go beyond reasonable bounds
        let safe_pos = cursor_display_pos.min(1000); // Reasonable terminal width limit

        // crossterm uses 0-based column indexing
        queue!(out, ResetColor, cursor::MoveToColumn(safe_pos as u16),).ok();
    }

    // fn move_cursor(&self, len: usize) {
    //     let mut stdout = std::io::stdout();
    //     let prompt_size = self.get_prompt().chars().count();
    //     queue!(
    //         stdout,
    //         ResetColor,
    //         cursor::MoveToColumn((prompt_size + len + 1) as u16),
    //     )
    //     .ok();
    // }

    fn print_prompt(&mut self, out: &mut StdoutLock<'static>) {
        debug!("print_prompt called - this will trigger full prompt redraw");
        let mut prompt = self.prompt.write();
        let prompt_mark = prompt.mark.clone();
        prompt.print_preprompt(out);
        out.write_all(b"\r").ok();
        out.write_all(prompt_mark.as_bytes()).ok();
        out.flush().ok();
    }

    fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.shell.cmd_history {
            match history.lock() {
                Ok(mut history) => {
                    history.search_word = None;
                    history.reset_index();
                }
                Err(e) => {
                    warn!(
                        "Failed to acquire command history lock for stopping history mode: {}",
                        e
                    );
                }
            }
        }
    }

    fn set_completions(&mut self) {
        if let Some(ref mut history) = self.shell.cmd_history {
            match history.lock() {
                Ok(history) => {
                    let comps = if self.input.is_empty() {
                        history.sorted(&dsh_frecency::SortMethod::Recent)
                    } else {
                        history.sort_by_match(self.input.as_str())
                    };

                    self.completion.set_completions(self.input.as_str(), comps);
                }
                Err(e) => {
                    warn!(
                        "Failed to acquire command history lock for completions: {}",
                        e
                    );
                    // Set empty completions as fallback
                    self.completion.set_completions(self.input.as_str(), vec![]);
                }
            }
        }
    }

    async fn handle_event(&mut self, ev: ShellEvent) -> Result<()> {
        match ev {
            ShellEvent::Input(input) => {
                if let Event::Key(key) = input {
                    self.handle_key_event(&key).await?
                }
                Ok(())
            }
            ShellEvent::ScreenResized => {
                let screen_size = terminal::size().unwrap_or_else(|e| {
                    warn!(
                        "Failed to get terminal size on resize: {}, keeping current size",
                        e
                    );
                    (self.columns as u16, self.lines as u16)
                });
                self.columns = screen_size.0 as usize;
                self.lines = screen_size.1 as usize;
                Ok(())
            }
        }
    }

    fn get_completion_from_history(&mut self, input: &str) -> Option<String> {
        if let Some(ref mut history) = self.shell.cmd_history {
            match history.lock() {
                Ok(history) => {
                    if let Some(entry) = history.search_prefix(input) {
                        self.input.completion = Some(entry.clone());
                        if entry.len() >= input.len() {
                            return Some(entry[input.len()..].to_string());
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to acquire command history lock for completion: {}",
                        e
                    );
                }
            }
        }
        None
    }

    pub fn print_input(&mut self, out: &mut StdoutLock<'static>, reset_completion: bool) {
        debug!("print_input called, reset_completion: {}", reset_completion);
        queue!(out, cursor::Hide).ok();
        let input = self.input.to_string();
        let prompt_display_width = display_width(&self.prompt.write().mark);
        debug!(
            "Current input: '{}', prompt_display_width: {}",
            input, prompt_display_width
        );

        let mut completion: Option<String> = None;
        let mut can_execute = false;
        if input.is_empty() || reset_completion {
            self.input.completion = None
        } else {
            completion = self.get_completion_from_history(&input);

            let mut match_index: Vec<usize> = Vec::new();

            // TODO refactor
            if let Ok(words) = self.input.get_words() {
                for (ref rule, ref span, current) in words {
                    let word = span.as_str();
                    if word.is_empty() {
                        continue;
                    }
                    if let Some(_found) = self.shell.environment.read().lookup(word) {
                        for pos in span.start()..span.end() {
                            // change color
                            match_index.push(pos);
                        }
                        if let Rule::argv0 = rule {
                            can_execute = true;
                        }
                    }

                    if current && completion.is_none() {
                        match rule {
                            Rule::argv0 => {
                                if let Some(file) = self.shell.environment.read().search(word) {
                                    if file.len() >= input.len() {
                                        completion = Some(file[input.len()..].to_string());
                                    }
                                    self.input.completion = Some(file);
                                    break;
                                } else if let Ok(Some(dir)) =
                                    completion::path_completion_prefix(word)
                                {
                                    if dirs::is_dir(&dir) {
                                        if dir.len() >= input.len() {
                                            completion = Some(dir[input.len()..].to_string());
                                        }
                                        self.input.completion = Some(dir.to_string());
                                        break;
                                    }
                                }
                            }
                            Rule::args => {
                                if let Ok(Some(path)) = completion::path_completion_prefix(word) {
                                    if path.len() >= word.len() {
                                        let part = path[word.len()..].to_string();
                                        completion = Some(path[word.len()..].to_string());

                                        if let Some((pre, post)) = self.input.split_current_pos() {
                                            self.input.completion =
                                                Some(pre.to_owned() + &part + post);
                                        } else {
                                            self.input.completion = Some(input + &part);
                                        }
                                        break;
                                    }
                                }
                            }

                            _ => {}
                        }
                    }
                }
            }
            self.input.match_index = Some(match_index);
        }

        self.input.can_execute = can_execute;

        // Clear the current line and redraw prompt mark + input
        queue!(out, Print("\r"), Clear(ClearType::CurrentLine)).ok();

        // Only redraw the prompt mark (not the full preprompt)
        let prompt = self.prompt.read();
        let prompt_mark = &prompt.mark;
        debug!("Redrawing prompt mark: '{}'", prompt_mark);
        queue!(out, Print(prompt_mark)).ok();

        // Print the input
        self.input.print(out);

        self.move_cursor_input_end(out);

        if let Some(completion) = completion {
            self.input.print_candidates(out, completion);
            self.move_cursor_input_end(out);
        }
        queue!(out, cursor::Show).ok();
    }

    async fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<()> {
        let redraw = true;
        let mut reset_completion = false;

        // Reset Ctrl+C state on any key input other than Ctrl+C
        if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
            self.ctrl_c_state.reset();
        }

        match (ev.code, ev.modifiers) {
            // history
            (KeyCode::Up, NONE) => {
                if self.completion.completion_mode() {
                    if let Some(item) = self.completion.backward() {
                        self.input
                            .reset_with_match_index(item.item.clone(), item.match_index.clone());
                    }
                } else {
                    self.set_completions();
                    if let Some(item) = self.completion.backward() {
                        self.input
                            .reset_with_match_index(item.item.clone(), item.match_index.clone());
                    }
                }
            }
            // history
            (KeyCode::Down, NONE) => {
                if self.completion.completion_mode() {
                    if let Some(item) = self.completion.forward() {
                        self.input
                            .reset_with_match_index(item.item.clone(), item.match_index.clone());
                    }
                }
            }
            (KeyCode::Left, NONE) => {
                if self.input.cursor() > 0 {
                    self.input.completion = None;
                    self.input.move_by(-1);
                    self.completion.clear();

                    // Only move cursor, don't redraw entire prompt
                    let mut out = std::io::stdout().lock();
                    self.move_cursor_input_end(&mut out);
                    out.flush().ok();
                    return Ok(());
                } else {
                    return Ok(());
                }
            }
            (KeyCode::Right, NONE) if self.input.completion.is_some() => {
                // TODO refactor
                if let Some(comp) = &self.input.completion {
                    let cursor = self.input.cursor();

                    if cursor >= comp.len() {
                        return Ok(());
                    }

                    if let Some((comp, post)) = comp[cursor..].split_once(' ') {
                        let mut comp = self.input.as_str().to_owned() + comp;
                        if !post.is_empty() {
                            comp += " ";
                        };
                        self.input.reset(comp.to_string());
                    } else {
                        self.input.reset(comp.to_string());
                        self.input.completion = None;
                    }
                }
                self.completion.clear();
            }
            (KeyCode::Right, NONE) => {
                if self.input.cursor() < self.input.len() {
                    self.input.move_by(1);
                    self.completion.clear();

                    // Only move cursor, don't redraw entire prompt
                    let mut out = std::io::stdout().lock();
                    self.move_cursor_input_end(&mut out);
                    out.flush().ok();
                    return Ok(());
                } else {
                    return Ok(());
                }
            }
            (KeyCode::Char(' '), NONE) => {
                // Handle abbreviation expansion before inserting space
                if let Some(word) = self.input.get_current_word_for_abbr() {
                    debug!("ABBR_EXPANSION: Found word for expansion: '{}'", word);
                    if let Some(expansion) = self.shell.environment.read().abbreviations.get(&word)
                    {
                        debug!(
                            "ABBR_EXPANSION: Found expansion for '{}': '{}'",
                            word, expansion
                        );
                        let expansion = expansion.clone();
                        if self.input.replace_current_word(&expansion) {
                            debug!(
                                "ABBR_EXPANSION: Successfully replaced '{}' with '{}'",
                                word, expansion
                            );
                            // Abbreviation was expanded, force redraw
                            reset_completion = true;
                        } else {
                            debug!("ABBR_EXPANSION: Failed to replace word '{}'", word);
                        }
                    } else {
                        debug!("ABBR_EXPANSION: No expansion found for word '{}'", word);
                        let abbrs = self.shell.environment.read().abbreviations.clone();
                        debug!("ABBR_EXPANSION: Available abbreviations: {:?}", abbrs);
                    }
                } else {
                    debug!("ABBR_EXPANSION: No word found for expansion at cursor position");
                }

                self.input.insert(' ');
                if self.completion.is_changed(self.input.as_str()) {
                    self.completion.clear();
                }
            }
            (KeyCode::Char(ch), NONE) => {
                self.input.insert(ch);
                if self.completion.is_changed(self.input.as_str()) {
                    self.completion.clear();
                }
            }
            (KeyCode::Char(ch), SHIFT) => {
                self.input.insert(ch);
                if self.completion.is_changed(self.input.as_str()) {
                    self.completion.clear();
                }
            }
            (KeyCode::Backspace, NONE) => {
                reset_completion = true;
                self.input.backspace();
                self.completion.clear();
                self.input.match_index = None;
            }
            (KeyCode::Tab, NONE) | (KeyCode::BackTab, NONE) => {
                let completion_query = match self.input.get_cursor_word() {
                    Ok(Some((_rule, span))) => Some(span.as_str()),
                    _ => None,
                };

                // Get prompt and input text for completion display
                let prompt_text = self.prompt.read().mark.clone();
                let input_text = self.input.to_string();

                debug!(
                    "Starting completion with prompt: '{}', input: '{}'",
                    prompt_text, input_text
                );

                // Use the new integrated completion engine
                let current_dir = std::env::current_dir().unwrap_or_default();
                let cursor_pos = self.input.cursor();

                debug!(
                    "Using IntegratedCompletionEngine for input: '{}' at position {}",
                    input_text, cursor_pos
                );

                let candidates = self
                    .integrated_completion
                    .complete(
                        &input_text,
                        cursor_pos,
                        &current_dir,
                        MAX_RESULT, // max candidates
                    )
                    .await;

                debug!(
                    "IntegratedCompletionEngine returned {} candidates. {:?}",
                    candidates.len(),
                    candidates,
                );

                let completion_result = if !candidates.is_empty() {
                    // Convert to completion format and show with skim
                    let completion_candidates: Vec<completion::Candidate> =
                        self.integrated_completion.to_candidates(candidates);

                    completion::select_item_with_skim(completion_candidates, completion_query)
                } else {
                    // Fall back to existing completion if no candidates from integrated engine
                    debug!(
                        "No candidates from IntegratedCompletionEngine, falling back to legacy completion. {:?} {:?}",
                        &self.input, &completion_query
                    );
                    completion::input_completion(
                        &self.input,
                        self,
                        completion_query,
                        &prompt_text,
                        &input_text,
                    )
                };

                if let Some(val) = completion_result {
                    debug!("Completion selected: '{}'", val);
                    if let Some(q) = completion_query {
                        self.input.backspacen(q.len());
                    }
                    self.input.insert_str(val.as_str());
                    debug!("Input after completion: '{}'", self.input.to_string());
                } else {
                    debug!("No completion selected");
                }

                // Force redraw after completion
                reset_completion = true;
                self.start_completion = true;
                debug!("Set start_completion flag to true and reset_completion to true");
            }
            (KeyCode::Enter, NONE) => {
                // Handle abbreviation expansion on Enter if cursor is at end of a word
                if let Some(word) = self.input.get_current_word_for_abbr() {
                    if let Some(expansion) = self.shell.environment.read().abbreviations.get(&word)
                    {
                        let expansion = expansion.clone();
                        if self.input.replace_current_word(&expansion) {
                            // Abbreviation was expanded - the input will be redrawn after command execution
                            debug!("Abbreviation '{}' expanded to '{}'", word, expansion);
                        }
                    }
                }

                self.input.completion.take();
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    self.completion.clear();
                    let shell_tmode = match tcgetattr(0) {
                        Ok(tmode) => tmode,
                        Err(e) => {
                            warn!(
                                "Failed to get terminal attributes: {}, using stored mode",
                                e
                            );
                            self.tmode.clone().unwrap_or_else(|| {
                                warn!("No stored terminal mode available, using default");
                                // Create a default Termios - this is a fallback that may not work perfectly
                                // but prevents crashes
                                unsafe { std::mem::zeroed() }
                            })
                        }
                    };
                    let mut ctx = Context::new(self.shell.pid, self.shell.pgid, shell_tmode, true);
                    match self
                        .shell
                        .eval_str(&mut ctx, self.input.to_string(), false)
                        .await
                    {
                        Ok(code) => {
                            debug!("exit: {} : {:?}", self.input.as_str(), code);
                        }
                        Err(err) => {
                            display_user_error(&err);
                        }
                    }
                    self.input.clear();
                }
                // After command execution, show new prompt
                let mut out = std::io::stdout().lock();
                self.print_prompt(&mut out);
                return Ok(());
            }
            (KeyCode::Enter, ALT) => {
                self.input.completion.take();
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    self.completion.clear();
                    let input = self.input.to_string();
                    let shell_tmode = match tcgetattr(0) {
                        Ok(tmode) => tmode,
                        Err(e) => {
                            warn!(
                                "Failed to get terminal attributes: {}, using stored mode",
                                e
                            );
                            self.tmode.clone().unwrap_or_else(|| {
                                warn!("No stored terminal mode available, using default");
                                unsafe { std::mem::zeroed() }
                            })
                        }
                    };
                    let mut ctx = Context::new(self.shell.pid, self.shell.pgid, shell_tmode, true);
                    if let Err(err) = self.shell.eval_str(&mut ctx, input, true).await {
                        display_user_error(&err);
                    }
                    self.input.clear();
                }
                // After command execution, show new prompt
                let mut out = std::io::stdout().lock();
                self.print_prompt(&mut out);
                return Ok(());
            }
            (KeyCode::Char('a'), CTRL) => {
                self.input.move_to_begin();
            }
            (KeyCode::Char('e'), CTRL) if self.input.completion.is_some() => {
                if let Some(comp) = &self.input.completion.take() {
                    self.input.reset(comp.to_string());
                }
                self.completion.clear();
            }
            (KeyCode::Char('e'), CTRL) => {
                self.input.move_to_end();
            }
            (KeyCode::Char('c'), CTRL) => {
                let mut out = std::io::stdout().lock();

                if self.ctrl_c_state.on_ctrl_c_pressed() {
                    // Second Ctrl+C - exit shell normally
                    execute!(out, Print("\r\nExiting shell...\r\n")).ok();
                    self.should_exit = true;
                    return Ok(());
                } else {
                    // First Ctrl+C - reset prompt + show message
                    execute!(
                        out,
                        Print("\r\n(Press Ctrl+C again within 3 seconds to exit)\r\n")
                    )
                    .ok();
                    self.print_prompt(&mut out);
                    self.input.clear();
                    return Ok(());
                }
            }
            (KeyCode::Char('l'), CTRL) => {
                let mut out = std::io::stdout().lock();
                execute!(out, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
                self.print_prompt(&mut out);
                self.input.clear();
                return Ok(());
            }
            (KeyCode::Char('d'), CTRL) => {
                let mut out = std::io::stdout().lock();
                execute!(out, Print("\r\nuse 'exit' to leave the shell\n")).ok();
                self.print_prompt(&mut out);
                self.input.clear();
                return Ok(());
            }
            (KeyCode::Char('r'), CTRL) => {
                self.select_history();
            }
            _ => {
                warn!("unsupported key event: {:?}", ev);
            }
        }

        // Unified output handling - single stdout lock for all output operations
        let mut out = std::io::stdout().lock();
        if redraw {
            debug!("Redrawing input, reset_completion: {}", reset_completion);
            self.print_input(&mut out, reset_completion);
            // Handle cursor positioning after output
            self.move_cursor_input_end(&mut out);
        }
        // Note: For cursor-only movements (redraw=false), cursor positioning
        // is handled directly in the key event handlers to avoid full redraw

        out.flush().ok();
        Ok(())
    }

    pub async fn run_interactive(&mut self) -> Result<()> {
        let mut reader = EventStream::new();

        self.setup();

        debug!(
            "shell setpgid pid:{:?} pgid:{:?}",
            self.shell.pid, self.shell.pgid
        );
        let _ = tcsetpgrp(SHELL_TERMINAL, self.shell.pgid).context("failed tcsetpgrp");
        self.tmode = match tcgetattr(SHELL_TERMINAL) {
            Ok(tmode) => Some(tmode),
            Err(e) => {
                warn!("Failed to get terminal attributes: {}", e);
                None
            }
        };
        {
            let mut out = std::io::stdout().lock();
            // start repl loop
            self.print_prompt(&mut out);
        }
        self.shell.check_job_state().await?;

        let mut save_history_delay = Delay::new(Duration::from_millis(10_000)).fuse();
        loop {
            let mut check_background_delay = Delay::new(Duration::from_millis(1000)).fuse();
            let mut event = reader.next().fuse();
            select! {
                _ = save_history_delay => {
                    if let Some(ref mut history) = self.shell.path_history {
                        let history = history.clone();
                        tokio::spawn(async move{
                            match history.lock() {
                                Ok(mut history) => {
                                    if let Err(e) = history.save() {
                                        warn!("Failed to save path history in background: {}", e);
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to acquire path history lock in background: {}", e);
                                }
                            }
                        });
                    }
                    if let Some(ref mut history) = self.shell.cmd_history {
                        let history = history.clone();
                        tokio::spawn(async move{
                            match history.lock() {
                                Ok(mut history) => {
                                    if let Err(e) = history.save() {
                                        warn!("Failed to save command history in background: {}", e);
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to acquire command history lock in background: {}", e);
                                }
                            }
                        });
                    }
                    save_history_delay = Delay::new(Duration::from_millis(10_000)).fuse();
                },

                _ = check_background_delay => {
                    self.check_background_jobs(true).await?;
                },
                maybe_event = event => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            if let Err(err) = self.handle_event(ShellEvent::Input(event)).await{
                                self.shell.print_error(format!("Error: {err:?}\r"));
                                break;
                            }
                        }
                        Some(Err(err)) => {
                            self.shell.print_error(format!("Error: {err:?}\r"));
                            break;
                        },
                        None => break,
                    }
                }
            };

            if self.start_completion {
                // show completion
                self.start_completion = false;
            }
            if self.should_exit || self.shell.exited.is_some() {
                debug!("Shell exiting normally");
                if !self.shell.wait_jobs.is_empty() {
                    // TODO show message
                }
                break;
            }
        }
        self.shell.kill_wait_jobs()?;
        Ok(())
    }

    pub fn select_history(&mut self) {
        let query = self.input.as_str();
        if let Some(ref mut history) = self.shell.cmd_history {
            match history.lock() {
                Ok(mut history) => {
                    let histories = history.sorted(&dsh_frecency::SortMethod::Recent);
                    if let Some(val) = completion::select_item_with_skim(
                        histories
                            .iter()
                            .map(|history| completion::Candidate::Basic(history.item.to_string()))
                            .collect(),
                        Some(query),
                    ) {
                        self.input.insert_str(val.as_str());
                    }
                    history.reset_index();
                }
                Err(e) => {
                    warn!(
                        "Failed to acquire command history lock for history selection: {}",
                        e
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_ctrl_c_state_single_press() {
        let mut state = CtrlCState::new();

        // First press returns false
        assert!(!state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 1);
        assert!(state.first_press_time.is_some());
    }

    #[test]
    fn test_ctrl_c_state_double_press_within_timeout() {
        let mut state = CtrlCState::new();

        // First press
        assert!(!state.on_ctrl_c_pressed());

        // Second press after short time
        thread::sleep(std::time::Duration::from_millis(100));
        assert!(state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 2);
    }

    #[test]
    fn test_ctrl_c_state_double_press_after_timeout() {
        let mut state = CtrlCState::new();

        // First press
        assert!(!state.on_ctrl_c_pressed());

        // Press after more than 3 seconds (treated as new first press)
        thread::sleep(std::time::Duration::from_secs(4));
        assert!(!state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 1);
    }

    #[test]
    fn test_ctrl_c_state_reset() {
        let mut state = CtrlCState::new();

        // First press
        assert!(!state.on_ctrl_c_pressed());

        // Reset
        state.reset();
        assert_eq!(state.press_count, 0);
        assert!(state.first_press_time.is_none());

        // Press after reset is treated as first press
        assert!(!state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 1);
    }
}
