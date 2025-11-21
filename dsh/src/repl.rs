use crate::completion::integrated::{CompletionResult, IntegratedCompletionEngine};
use crate::completion::{self, Completion, MAX_RESULT};
use crate::dirs;
use crate::environment::Environment;
use crate::errors::display_user_error;
use crate::history::FrecencyHistory;
use crate::input::{Input, InputConfig, display_width};
use crate::parser::Rule;
use crate::prompt::Prompt;
use crate::shell::{SHELL_TERMINAL, Shell};
use crate::suggestion::{
    AiSuggestionBackend, InputPreferences, SuggestionBackend, SuggestionEngine, SuggestionState,
};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Context as _;
use anyhow::Result;
use arboard::Clipboard;
use crossterm::cursor;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::queue;
use crossterm::style::{Print, ResetColor};
use crossterm::terminal::{self, Clear, ClearType, enable_raw_mode};
use dsh_openai::{ChatGptClient, OpenAiConfig};
use dsh_types::Context;
use futures::StreamExt;
use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::tcsetpgrp;
use parking_lot::Mutex as ParkingMutex;
use parking_lot::RwLock;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval_at};
use tracing::{debug, warn};

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
const AI_SUGGESTION_REFRESH_MS: u64 = 150;

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
    // Cached prompt mark and its display width to avoid recomputation on each redraw
    prompt_mark_cache: String,
    prompt_mark_width: usize,
    ctrl_c_state: CtrlCState,
    should_exit: bool,
    last_command_time: Option<Instant>,
    // short-term cache for history-based completion to reduce lock/sort frequency
    history_cache_prefix: String,
    history_cache_time: Option<Instant>,
    history_cache_ttl: Duration,
    history_cache_sorted_recent: Option<Vec<dsh_frecency::ItemStats>>,
    history_cache_match_sorted: Option<Vec<dsh_frecency::ItemStats>>,
    suggestion_engine: SuggestionEngine,
    active_suggestion: Option<SuggestionState>,
    input_preferences: InputPreferences,
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

        let prompt_mark_cache = prompt.read().mark.clone();
        let prompt_mark_width = display_width(&prompt_mark_cache);

        let envronment = Arc::clone(&shell.environment);
        let input_preferences = envronment.read().input_preferences();
        let mut suggestion_engine = SuggestionEngine::new();
        if let Some(ai_backend) = Self::build_ai_backend(&envronment) {
            suggestion_engine.set_ai_backend(Some(ai_backend));
        }
        suggestion_engine.set_preferences(input_preferences);
        Repl {
            shell,
            input: Input::new(input_config),
            columns: 0,
            lines: 0,
            tmode: None,
            history_search: None,
            start_completion: false,
            completion: Completion::new(),
            integrated_completion: IntegratedCompletionEngine::new(envronment),
            prompt,
            prompt_mark_cache,
            prompt_mark_width,
            ctrl_c_state: CtrlCState::new(),
            should_exit: false,
            last_command_time: None,
            history_cache_prefix: String::new(),
            history_cache_time: None,
            history_cache_ttl: Duration::from_millis(300),
            history_cache_sorted_recent: None,
            history_cache_match_sorted: None,
            suggestion_engine,
            active_suggestion: None,
            input_preferences,
        }
    }

    fn build_ai_backend(
        environment: &Arc<RwLock<Environment>>,
    ) -> Option<Arc<dyn SuggestionBackend + Send + Sync>> {
        let env_handle = Arc::clone(environment);
        let config = OpenAiConfig::from_getter(|key| {
            let value = {
                let guard = env_handle.read();
                guard.get_var(key)
            };
            value.or_else(|| std::env::var(key).ok())
        });

        config.api_key()?;

        match ChatGptClient::try_from_config(&config) {
            Ok(client) => {
                let backend: Arc<dyn SuggestionBackend + Send + Sync> =
                    Arc::new(AiSuggestionBackend::new(client));
                Some(backend)
            }
            Err(err) => {
                warn!("Failed to initialize AI suggestion backend: {err:?}");
                None
            }
        }
    }

    fn setup(&mut self) {
        let screen_size = terminal::size().unwrap_or_else(|e| {
            warn!("Failed to get terminal size: {}, using default 80x24", e);
            (80, 24)
        });
        self.columns = screen_size.0 as usize;

        // Initialize integrated completion engine
        debug!("Initializing integrated completion engine (this may use cached JSON data)...");
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
            // Process background output for completed jobs
            for mut job in jobs {
                if !job.foreground {
                    job.check_background_all_output().await?;
                }
            }

            // Batch all output operations with a single terminal renderer
            let mut renderer = TerminalRenderer::new();
            let mut output_buffer = String::new();

            // Check remaining jobs in wait_jobs for status messages
            // Note: Completed jobs are no longer in self.shell.wait_jobs since they were removed
            // by check_job_state, so we only need to check the remaining active jobs.
            for job in &self.shell.wait_jobs {
                if !job.foreground && output {
                    output_buffer.push_str(&format!(
                        "\rdsh: job {} '{}' {}\n",
                        job.job_id, job.cmd, job.state
                    ));
                }
            }

            if !output_buffer.is_empty() {
                renderer.write_all(output_buffer.as_bytes())?;
                self.print_prompt(&mut renderer);
                renderer.flush()?;
            }
        }
        Ok(())
    }

    fn save_history(&mut self) {
        Self::save_single_history_helper(&mut self.shell.cmd_history, "command");
        Self::save_single_history_helper(&mut self.shell.path_history, "path");
    }

    fn save_single_history_helper(
        history: &mut Option<Arc<ParkingMutex<FrecencyHistory>>>,
        history_type: &str,
    ) {
        if let Some(history) = history {
            if let Some(mut history_guard) = history.try_lock() {
                // Only save if there are changes
                if let Some(ref store) = history_guard.store {
                    if store.changed {
                        if let Err(e) = history_guard.save() {
                            warn!("Failed to save {} history: {}", history_type, e);
                        } else {
                            // debug!("{} history saved successfully", history_type);
                        }
                    } else {
                        // debug!("{} history unchanged, skipping save", history_type);
                    }
                }
            } else {
                // debug!("{} history is locked, skipping save", history_type);
            }
        }
    }

    fn save_history_periodic(&mut self) {
        // Save both command and path history if they have changes
        Self::save_single_history_helper(&mut self.shell.cmd_history, "command");
        Self::save_single_history_helper(&mut self.shell.path_history, "path");
    }

    fn move_cursor_input_end<W: Write>(&self, out: &mut W) {
        let prompt_display_width = self.prompt_mark_width;
        // cache locally to avoid duplicate computation chains
        let input_cursor_width = self.input.cursor_display_width();
        let mut cursor_display_pos = prompt_display_width + input_cursor_width;

        // debug!(
        //     "move_cursor_input_end: prompt_mark='{}', prompt_width={}, input_cursor_width={}, final_pos={}",
        //     self.prompt_mark_cache, prompt_display_width, input_cursor_width, cursor_display_pos
        // );
        // debug!(
        //     "move_cursor_input_end: input_text='{}', input_cursor_pos={}",
        //     self.input.as_str(),
        //     self.input.cursor()
        // );

        // bound to current terminal columns if available
        if self.columns > 0 {
            cursor_display_pos = cursor_display_pos.min(self.columns.saturating_sub(1));
        } else {
            cursor_display_pos = cursor_display_pos.min(1000);
        }

        // crossterm uses 0-based column indexing
        queue!(
            out,
            ResetColor,
            cursor::MoveToColumn(cursor_display_pos as u16)
        )
        .ok();
    }

    /// Move cursor relatively on the input line given previous and new display positions
    fn move_cursor_relative(
        &self,
        out: &mut impl Write,
        prev_display_pos: usize,
        new_display_pos: usize,
    ) {
        if new_display_pos == prev_display_pos {
            return;
        }
        if new_display_pos > prev_display_pos {
            let delta = (new_display_pos - prev_display_pos) as u16;
            queue!(out, cursor::MoveRight(delta)).ok();
        } else {
            let delta = (prev_display_pos - new_display_pos) as u16;
            queue!(out, cursor::MoveLeft(delta)).ok();
        }
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

    fn print_prompt(&mut self, out: &mut impl Write) {
        // debug!("print_prompt called - full preprompt + mark redraw");

        // Execute pre-prompt hooks
        if let Err(e) = self.shell.exec_pre_prompt_hooks() {
            debug!("Error executing pre-prompt hooks: {}", e);
        }

        let mut prompt = self.prompt.write();
        // draw preprompt only here (initial or after command/bg output)
        prompt.print_preprompt(out);
        // update cached mark and width in case mark changed
        self.prompt_mark_cache = prompt.mark.clone();
        self.prompt_mark_width = display_width(&self.prompt_mark_cache);
        // draw mark only (defer flushing to caller for batching)
        out.write_all(b"\r").ok();
        out.write_all(self.prompt_mark_cache.as_bytes()).ok();
        // no out.flush() here
    }

    fn sync_input_preferences(&mut self) {
        let prefs = self.shell.environment.read().input_preferences();
        if prefs != self.input_preferences {
            self.input_preferences = prefs;
            self.suggestion_engine.set_preferences(prefs);
        }
    }

    fn refresh_inline_suggestion(&mut self) -> bool {
        if self.input.completion.is_some() {
            let had_suggestion = self.active_suggestion.take().is_some();
            return had_suggestion;
        }

        self.sync_input_preferences();
        let history_ref = self.shell.cmd_history.as_ref();
        let next =
            self.suggestion_engine
                .predict(self.input.as_str(), self.input.cursor(), history_ref);

        let changed = match (&self.active_suggestion, &next) {
            (Some(prev), Some(curr)) => prev.full != curr.full || prev.source != curr.source,
            (None, Some(_)) | (Some(_), None) => true,
            (None, None) => false,
        };

        self.active_suggestion = next;
        changed
    }

    fn suggestion_suffix(&self, input: &str) -> Option<String> {
        let suggestion = self.active_suggestion.as_ref()?;
        if !suggestion.full.starts_with(input) || suggestion.full.len() <= input.len() {
            return None;
        }
        Some(suggestion.full[input.len()..].to_string())
    }

    fn accept_active_suggestion(&mut self) -> bool {
        let suggestion = match self.active_suggestion.take() {
            Some(state) => state,
            None => return false,
        };

        let current = self.input.as_str().to_string();
        if !suggestion.full.starts_with(&current) || suggestion.full.len() <= current.len() {
            return false;
        }

        let suffix = &suggestion.full[current.len()..];
        self.input.insert_str(suffix);
        true
    }

    fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
        {
            history.search_word = None;
            history.reset_index();
        }
        // If we can't get the lock, we just won't be able to stop history mode - no warning needed
    }

    fn set_completions(&mut self) {
        let now = Instant::now();
        let input_str = self.input.as_str().to_string();
        let is_empty = input_str.is_empty();

        // Try using cache first when TTL is valid and prefix unchanged
        if let Some(last_time) = self.history_cache_time
            && now.duration_since(last_time) <= self.history_cache_ttl
            && self.history_cache_prefix == input_str
        {
            if is_empty {
                if let Some(ref comps) = self.history_cache_sorted_recent {
                    self.completion
                        .set_completions(self.input.as_str(), comps.clone());
                    return;
                }
            } else if let Some(ref comps) = self.history_cache_match_sorted {
                self.completion
                    .set_completions(self.input.as_str(), comps.clone());
                return;
            }
        }

        // Fallback to computing with lock, and refresh cache if successful
        if let Some(ref mut history) = self.shell.cmd_history {
            if let Some(history) = history.try_lock() {
                // If store changed, invalidate cache
                let changed = history.store.as_ref().map(|s| s.changed).unwrap_or(false);
                if changed {
                    self.history_cache_sorted_recent = None;
                    self.history_cache_match_sorted = None;
                    self.history_cache_time = None;
                }

                let comps = if is_empty {
                    let list = history.sorted(&dsh_frecency::SortMethod::Recent);
                    self.history_cache_sorted_recent = Some(list.clone());
                    list
                } else {
                    let list = history.sort_by_match(&input_str);
                    self.history_cache_match_sorted = Some(list.clone());
                    list
                };

                self.history_cache_prefix = input_str;
                self.history_cache_time = Some(now);

                self.completion.set_completions(self.input.as_str(), comps);
            } else {
                // If we can't get the lock immediately, try using the cache if available, otherwise empty
                if let Some(last_time) = self.history_cache_time
                    && now.duration_since(last_time) <= self.history_cache_ttl
                {
                    if is_empty {
                        if let Some(ref comps) = self.history_cache_sorted_recent {
                            self.completion
                                .set_completions(self.input.as_str(), comps.clone());
                            return; // Exit early since we used the cache
                        }
                    } else if let Some(ref comps) = self.history_cache_match_sorted {
                        self.completion
                            .set_completions(self.input.as_str(), comps.clone());
                        return; // Exit early since we used the cache
                    }
                }
                // Set empty completions as fallback when lock is not available
                self.completion.set_completions(self.input.as_str(), vec![]);
                // No warning here since this is expected during high contention
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
        let now = Instant::now();
        // Try cached match-sorted list first if still fresh and prefix unchanged
        if let Some(last_time) = self.history_cache_time
            && now.duration_since(last_time) <= self.history_cache_ttl
            && self.history_cache_prefix.starts_with(input)
            && let Some(ref list) = self.history_cache_match_sorted
            && let Some(top) = list.iter().find(|it| it.item.starts_with(input))
        {
            let entry = top.item.clone();
            self.input.completion = Some(entry.clone());
            if entry.len() >= input.len() {
                return Some(entry[input.len()..].to_string());
            }
        }

        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(history) = history.try_lock()
            && let Some(entry) = history.search_prefix(input)
        {
            self.input.completion = Some(entry.clone());
            if entry.len() >= input.len() {
                return Some(entry[input.len()..].to_string());
            }
        }
        // If we can't get the lock, completion just won't be available - no warning needed
        None
    }

    pub fn print_input(
        &mut self,
        out: &mut impl Write,
        reset_completion: bool,
        refresh_suggestion: bool,
    ) {
        // debug!("print_input called, reset_completion: {}", reset_completion);
        queue!(out, cursor::Hide).ok();
        let input = self.input.to_string();
        let _prompt_display_width = self.prompt_mark_width; // cached at new()/print_prompt()
        // debug!(
        //     "Current input: '{}', prompt_display_width: {}",
        //     input, _prompt_display_width
        // );

        let mut completion: Option<String> = None;
        let mut can_execute = false;
        let mut show_ai_pending = false;
        if input.is_empty() || reset_completion {
            self.input.completion = None;
            self.input.color_ranges = None;
        } else {
            completion = self.get_completion_from_history(&input);

            let mut color_ranges: Vec<(usize, usize, crate::input::ColorType)> = Vec::new();

            // TODO refactor
            if let Ok(words) = self.input.get_words() {
                for (ref rule, ref span, current) in words {
                    let word = span.as_str();
                    if word.is_empty() {
                        continue;
                    }

                    let word_is_valid = self.command_is_valid(word);

                    match rule {
                        Rule::argv0 => {
                            // For command names (argv0), color based on existence
                            let color_type = if word_is_valid {
                                crate::input::ColorType::CommandExists
                            } else {
                                crate::input::ColorType::CommandNotExists
                            };
                            color_ranges.push((span.start(), span.end(), color_type));

                            if word_is_valid {
                                can_execute = true;
                            }

                            // Completion logic for command names
                            if current && completion.is_none() {
                                if let Some(file) = self.shell.environment.read().search(word) {
                                    if file.len() >= input.len() {
                                        completion = Some(file[input.len()..].to_string());
                                    }
                                    self.input.completion = Some(file);
                                    break;
                                } else if let Ok(Some(dir)) =
                                    completion::path_completion_prefix(word)
                                    && dirs::is_dir(&dir)
                                {
                                    if dir.len() >= input.len() {
                                        completion = Some(dir[input.len()..].to_string());
                                    }
                                    self.input.completion = Some(dir.to_string());
                                    break;
                                }
                            }
                        }
                        Rule::args => {
                            // For arguments, use cyan color
                            color_ranges.push((
                                span.start(),
                                span.end(),
                                crate::input::ColorType::Argument,
                            ));

                            // Completion logic for arguments
                            if current
                                && completion.is_none()
                                && let Ok(Some(path)) = completion::path_completion_prefix(word)
                                && path.len() >= word.len()
                            {
                                let part = path[word.len()..].to_string();
                                completion = Some(path[word.len()..].to_string());

                                if let Some((pre, post)) = self.input.split_current_pos() {
                                    self.input.completion = Some(pre.to_owned() + &part + post);
                                } else {
                                    self.input.completion = Some(input.clone() + &part);
                                }
                                break;
                            }
                        }
                        _ => {
                            // For other rule types, leave them with default color
                        }
                    }
                }
            }
            self.input.color_ranges = Some(color_ranges);
        }

        self.input.can_execute = can_execute;

        if completion.is_none() {
            if refresh_suggestion {
                self.refresh_inline_suggestion();
            }
            show_ai_pending =
                self.active_suggestion.is_none() && self.suggestion_engine.ai_pending();
        } else {
            self.active_suggestion = None;
        }

        let ghost_suffix = if completion.is_none() {
            self.suggestion_suffix(&input)
        } else {
            None
        };

        // Clear the current line and redraw prompt mark + input
        queue!(out, Print("\r"), Clear(ClearType::CurrentLine)).ok();

        // Only redraw the prompt mark (not the full preprompt)
        // Use cached prompt mark without re-locking prompt
        // debug!("Redrawing prompt mark: '{}'", self.prompt_mark_cache);
        queue!(out, Print(self.prompt_mark_cache.as_str())).ok();

        // Print the input
        self.input.print(out, ghost_suffix.as_deref());

        if show_ai_pending {
            queue!(out, Print(" ⧗")).ok();
        }

        self.move_cursor_input_end(out);

        if let Some(completion) = completion {
            self.input.print_candidates(out, completion);
            // reuse cached cursor width implicitly via move_cursor_input_end recomputation; avoid extra heavy work here
            self.move_cursor_input_end(out);
        }
        queue!(out, cursor::Show).ok();
    }

    async fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<()> {
        let redraw = true;
        let mut reset_completion = false;
        // compute previous and new cursor display positions for relative move
        let prompt_w = self.prompt_mark_width;
        // compute once per event to avoid duplicate width computation
        let prev_cursor_disp = prompt_w + self.input.cursor_display_width();

        // Reset Ctrl+C state on any key input other than Ctrl+C
        if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
            self.ctrl_c_state.reset();
        }

        match (ev.code, ev.modifiers) {
            // history
            (KeyCode::Up, NONE) => {
                if self.completion.completion_mode() {
                    if let Some(item) = self.completion.backward() {
                        // Convert match_index to color_ranges (for backward compatibility, assume all matches are commands that exist)
                        let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
                            .match_index
                            .iter()
                            .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
                            .collect();
                        self.input
                            .reset_with_color_ranges(item.item.clone(), color_ranges);
                    }
                } else {
                    self.set_completions();
                    if let Some(item) = self.completion.backward() {
                        // Convert match_index to color_ranges (for backward compatibility, assume all matches are commands that exist)
                        let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
                            .match_index
                            .iter()
                            .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
                            .collect();
                        self.input
                            .reset_with_color_ranges(item.item.clone(), color_ranges);
                    }
                }
            }
            // history
            (KeyCode::Down, NONE) => {
                if self.completion.completion_mode()
                    && let Some(item) = self.completion.forward()
                {
                    // Convert match_index to color_ranges (for backward compatibility, assume all matches are commands that exist)
                    let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
                        .match_index
                        .iter()
                        .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
                        .collect();
                    self.input
                        .reset_with_color_ranges(item.item.clone(), color_ranges);
                }
            }
            (KeyCode::Left, modifiers) if !modifiers.contains(CTRL) => {
                if self.input.cursor() > 0 {
                    self.input.completion = None;
                    self.input.move_by(-1);
                    self.completion.clear();

                    // Move cursor relatively, ensure cursor is visible in fast path
                    let mut renderer = TerminalRenderer::new();
                    let new_disp = self.prompt_mark_width + self.input.cursor_display_width();
                    self.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
                    queue!(renderer, cursor::Show).ok();
                    renderer.flush().ok();
                    return Ok(());
                } else {
                    return Ok(());
                }
            }
            (KeyCode::Right, modifiers)
                if self.active_suggestion.is_some()
                    && self.input.completion.is_none()
                    && self.input.cursor() == self.input.len()
                    && !modifiers.contains(CTRL) =>
            {
                if self.accept_active_suggestion() {
                    self.completion.clear();
                    reset_completion = true;
                }
            }
            (KeyCode::Right, modifiers)
                if self.input.completion.is_some() && !modifiers.contains(CTRL) =>
            {
                // TODO refactor
                if let Some(completion) = &self.input.completion {
                    let cursor = self.input.cursor();
                    let completion_chars = completion.chars().count();

                    if cursor >= completion_chars {
                        return Ok(());
                    }

                    let suffix_byte_index = completion
                        .char_indices()
                        .nth(cursor)
                        .map(|(idx, _)| idx)
                        .unwrap_or_else(|| completion.len());

                    if suffix_byte_index >= completion.len() {
                        return Ok(());
                    }

                    let suffix = &completion[suffix_byte_index..];

                    if let Some((fragment, post)) = suffix.split_once(' ') {
                        let mut new_input = self.input.as_str().to_owned();
                        new_input.push_str(fragment);
                        if !post.is_empty() {
                            new_input.push(' ');
                        }
                        self.input.reset(new_input);
                    } else {
                        self.input.reset(completion.to_string());
                        self.input.completion = None;
                    }
                }
                self.completion.clear();
            }
            (KeyCode::Right, modifiers) if !modifiers.contains(CTRL) => {
                if self.input.cursor() < self.input.len() {
                    self.input.move_by(1);
                    self.completion.clear();

                    // Move cursor relatively, ensure cursor is visible in fast path
                    let mut renderer = TerminalRenderer::new();
                    let new_disp = self.prompt_mark_width + self.input.cursor_display_width();
                    self.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
                    queue!(renderer, cursor::Show).ok();
                    renderer.flush().ok();
                    return Ok(());
                } else {
                    return Ok(());
                }
            }
            (KeyCode::Char('f'), CTRL)
                if self.active_suggestion.is_some()
                    && self.input.completion.is_none()
                    && self.input.cursor() == self.input.len() =>
            {
                if self.accept_active_suggestion() {
                    self.completion.clear();
                    reset_completion = true;
                }
            }
            (KeyCode::Char(' '), NONE) => {
                // Handle abbreviation expansion before inserting space
                if let Some(word) = self.input.get_current_word_for_abbr() {
                    // debug!("ABBR_EXPANSION: Found word for expansion: '{}'", word);
                    if let Some(expansion) = self.shell.environment.read().abbreviations.get(&word)
                    {
                        // debug!(
                        //     "ABBR_EXPANSION: Found expansion for '{}': '{}'",
                        //     word, expansion
                        // );
                        let expansion = expansion.clone();
                        if self.input.replace_current_word(&expansion) {
                            // debug!(
                            //     "ABBR_EXPANSION: Successfully replaced '{}' with '{}'",
                            //     word, expansion
                            // );
                            // Abbreviation was expanded, force redraw
                            reset_completion = true;
                        } else {
                            // debug!("ABBR_EXPANSION: Failed to replace word '{}'", word);
                        }
                    } else {
                        // debug!("ABBR_EXPANSION: No expansion found for word '{}'", word);
                        let _abbrs = self.shell.environment.read().abbreviations.clone();
                        // debug!("ABBR_EXPANSION: Available abbreviations: {:?}", _abbrs);
                    }
                } else {
                    // debug!("ABBR_EXPANSION: No word found for expansion at cursor position");
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
                self.input.color_ranges = None;
            }
            (KeyCode::Tab, NONE) | (KeyCode::BackTab, NONE) => {
                // Extract the current word at cursor position for completion query
                let completion_query_owned = match self.input.get_cursor_word() {
                    Ok(Some((_rule, span))) => Some(span.as_str().to_string()),
                    _ => self.input.get_completion_word_fallback(),
                };
                let completion_query = completion_query_owned.as_deref();
                let removal_len = completion_query_owned
                    .as_ref()
                    .map(|query| query.chars().count());

                // Get the current prompt text and input text for completion display context
                let prompt_text = self.prompt.read().mark.clone();
                let input_text = self.input.to_string();

                debug!(
                    "TAB completion starting with prompt: '{}', input: '{}', query: '{:?}'",
                    prompt_text, input_text, completion_query
                );

                // Use the new integrated completion engine with current directory context
                let current_dir = self.prompt.read().current_path().to_path_buf();
                let cursor_pos = self.input.cursor();

                debug!(
                    "Using IntegratedCompletionEngine for input: '{}' at position {}",
                    input_text, cursor_pos
                );

                // Get completion candidates from the integrated engine
                let CompletionResult {
                    candidates: engine_candidates,
                    framework: completion_framework,
                } = self
                    .integrated_completion
                    .complete(
                        &input_text,
                        cursor_pos,
                        &current_dir,
                        MAX_RESULT, // maximum number of candidates to return
                    )
                    .await;

                debug!(
                    "IntegratedCompletionEngine returned {} candidates (framework: {:?})",
                    engine_candidates.len(),
                    completion_framework
                );
                for (i, candidate) in engine_candidates.iter().enumerate() {
                    debug!("Integrated engine candidate {}: {:?}", i, candidate);
                }

                // Attempt to get completion result
                // First try with integrated completion engine, then fall back to legacy system
                let completion_result = if !engine_candidates.is_empty() {
                    // If integrated engine returned candidates, show them with skim selector
                    let completion_candidates: Vec<completion::Candidate> =
                        self.integrated_completion.to_candidates(engine_candidates);

                    debug!(
                        "Converted to {} UI candidates for {:?}",
                        completion_candidates.len(),
                        completion_framework
                    );
                    for (i, candidate) in completion_candidates.iter().enumerate() {
                        debug!("Skim UI candidate {}: {:?}", i, candidate);
                    }

                    completion::select_completion_items_with_framework(
                        completion_candidates,
                        completion_query,
                        &prompt_text,
                        &input_text,
                        crate::completion::CompletionConfig::default(),
                        completion_framework,
                    )
                } else {
                    debug!(
                        "No candidates from IntegratedCompletionEngine, falling back to legacy completion"
                    );
                    // If no candidates from integrated engine, fall back to legacy completion system
                    // This handles path completion, command completion from PATH, etc.
                    completion::input_completion(
                        &self.input,
                        self,
                        completion_query,
                        &prompt_text,
                        &input_text,
                    )
                };

                // Process the completion result
                if let Some(val) = completion_result {
                    debug!("Completion selected: '{}'", val);
                    // For history candidates (indicated by clock emoji), replace entire input
                    let is_history_candidate = val.starts_with("🕒 ");
                    if is_history_candidate {
                        let command = val[3..].trim(); // Remove the clock emoji and any extra spaces
                        self.input.reset(command.to_string());
                    } else {
                        // For regular completions, replace the query part with the selected value
                        if let Some(len) = removal_len {
                            self.input.backspacen(len); // Remove the original query text
                        }
                        self.input.insert_str(val.as_str()); // Insert the completion
                    }
                    debug!("Input after completion: '{}'", self.input.to_string());
                } else {
                    // No completion was selected - this happens when:
                    // 1. No candidates were found (empty candidate list returns None immediately)
                    // 2. User cancelled the completion interface (e.g. pressed ESC in skim)
                    // 3. User made no selection from the completion list
                    debug!("No completion selected");
                    // In this case, the input remains unchanged and no error is shown to user
                    // This is the "silent failure" behavior when no matches are found
                }

                // Force a redraw after completion to update the display
                reset_completion = true;
                self.start_completion = true;
                debug!("Set start_completion flag to true and reset_completion to true");

                // Note: When no matches are found, no UI is shown and no error is displayed to user.
                // The integrated completion engine returns an empty vector when no candidates match,
                // which immediately results in a fallback to legacy completion.
                // If legacy completion also finds no matches, completion::input_completion returns None,
                // leading to the "No completion selected" case above.
            }
            (KeyCode::Enter, NONE) => {
                // Handle abbreviation expansion on Enter if cursor is at end of a word
                if let Some(word) = self.input.get_current_word_for_abbr()
                    && let Some(expansion) = self.shell.environment.read().abbreviations.get(&word)
                {
                    let expansion = expansion.clone();
                    if self.input.replace_current_word(&expansion) {
                        // Abbreviation was expanded - the input will be redrawn after command execution
                        debug!("Abbreviation '{}' expanded to '{}'", word, expansion);
                    }
                }

                self.input.completion.take();
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    self.completion.clear();
                    let shell_tmode = self.tmode.clone().unwrap_or_else(|| {
                        warn!("No stored terminal mode available, using default");
                        // Create a default Termios - this is a fallback that may not work perfectly
                        // but prevents crashes
                        unsafe { std::mem::zeroed() }
                    });
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
                            display_user_error(&err, false);
                        }
                    }
                    self.input.clear();
                    self.active_suggestion = None;
                    self.last_command_time = Some(Instant::now());
                }
                // After command execution, show new prompt
                let mut renderer = TerminalRenderer::new();
                self.print_prompt(&mut renderer);
                renderer.flush().ok();
                return Ok(());
            }
            (KeyCode::Enter, ALT) => {
                self.input.completion.take();
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    self.completion.clear();
                    let input = self.input.to_string();
                    let shell_tmode = self.tmode.clone().unwrap_or_else(|| {
                        warn!("No stored terminal mode available, using default");
                        unsafe { std::mem::zeroed() }
                    });
                    let mut ctx = Context::new(self.shell.pid, self.shell.pgid, shell_tmode, true);
                    if let Err(err) = self.shell.eval_str(&mut ctx, input, true).await {
                        display_user_error(&err, false);
                    }
                    self.input.clear();
                    self.active_suggestion = None;
                }
                // After command execution, show new prompt
                let mut renderer = TerminalRenderer::new();
                self.print_prompt(&mut renderer);
                renderer.flush().ok();
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
                let mut renderer = TerminalRenderer::new();

                if self.ctrl_c_state.on_ctrl_c_pressed() {
                    // Second Ctrl+C - exit shell normally
                    // queue message and flush once here
                    queue!(renderer, Print("\r\nExiting shell...\r\n")).ok();
                    renderer.flush().ok();
                    self.should_exit = true;
                    return Ok(());
                } else {
                    // First Ctrl+C - reset prompt + show message
                    // queue message and defer flushing until after prompt
                    queue!(
                        renderer,
                        Print("\r\n(Press Ctrl+C again within 3 seconds to exit)\r\n")
                    )
                    .ok();
                    self.print_prompt(&mut renderer);
                    renderer.flush().ok();
                    self.input.clear();
                    self.active_suggestion = None;
                    return Ok(());
                }
            }
            (KeyCode::Char('l'), CTRL) => {
                let mut renderer = TerminalRenderer::new();
                queue!(renderer, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
                self.print_prompt(&mut renderer);
                renderer.flush().ok();
                self.input.clear();
                self.active_suggestion = None;
                return Ok(());
            }
            (KeyCode::Char('d'), CTRL) => {
                let mut renderer = TerminalRenderer::new();
                queue!(renderer, Print("\r\nuse 'exit' to leave the shell\n")).ok();
                self.print_prompt(&mut renderer);
                renderer.flush().ok();
                self.input.clear();
                self.active_suggestion = None;
                return Ok(());
            }
            (KeyCode::Char('r'), CTRL) => {
                self.select_history();
            }
            (KeyCode::Char('v'), CTRL) => {
                // Paste clipboard content at current cursor position
                if let Ok(mut clipboard) = Clipboard::new()
                    && let Ok(content) = clipboard.get_text()
                {
                    // Insert the clipboard content at the current cursor position
                    self.input.insert_str(&content);
                    self.completion.clear();
                }
            }
            _ => {
                warn!("unsupported key event: {:?}", ev);
            }
        }

        if redraw {
            debug!("Redrawing input, reset_completion: {}", reset_completion);
            let mut renderer = TerminalRenderer::new();
            self.print_input(&mut renderer, reset_completion, true);
            renderer.flush().ok();
        }
        // Note: For cursor-only movements (redraw=false), cursor positioning
        // is handled directly in the key event handlers to avoid full redraw
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
            let mut renderer = TerminalRenderer::new();
            // start repl loop
            self.print_prompt(&mut renderer);
            // ensure preprompt + mark are flushed on initial draw
            renderer.flush().ok();
        }
        self.shell.check_job_state().await?;

        let _last_save_time = Instant::now();
        let mut background_interval = interval_at(
            TokioInstant::now() + Duration::from_millis(1000),
            Duration::from_millis(1000),
        );
        background_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut ai_refresh_interval = interval_at(
            TokioInstant::now() + Duration::from_millis(AI_SUGGESTION_REFRESH_MS),
            Duration::from_millis(AI_SUGGESTION_REFRESH_MS),
        );
        ai_refresh_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = background_interval.tick() => {
                    // Save history every 30 seconds if there have been changes
                    self.save_history_periodic();
                    self.check_background_jobs(true).await?;
                },
                _ = ai_refresh_interval.tick() => {
                    if self.input_preferences.ai_backfill
                        && self.input.completion.is_none()
                        && self.refresh_inline_suggestion()
                    {
                        let mut renderer = TerminalRenderer::new();
                        self.print_input(&mut renderer, false, false);
                        renderer.flush().ok();
                    }
                }
                maybe_event = reader.next() => {
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
            if let Some(mut history) = history.try_lock() {
                let histories = history.sorted(&dsh_frecency::SortMethod::Recent);
                if let Some(val) = completion::select_item_with_skim(
                    histories
                        .into_iter()
                        .map(|history| completion::Candidate::Basic(history.item))
                        .collect(),
                    Some(query),
                ) {
                    // Replace current input with the selected history command
                    self.input.reset(val);
                }
                history.reset_index();
            } else {
                warn!(
                    "Failed to acquire command history lock for history selection - lock is busy"
                );
            }
        }
    }

    fn command_is_valid(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }

        {
            let env = self.shell.environment.read();
            if env.lookup(word).is_some() {
                return true;
            }

            if env.alias.contains_key(word) {
                return true;
            }
        }

        if dsh_builtin::get_command(word).is_some() {
            return true;
        }

        self.shell.lisp_engine.borrow().is_export(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::shell::Shell;
    use std::thread;

    #[tokio::test]
    async fn background_interval_ticks_even_with_busy_events() {
        let mut interval = interval_at(TokioInstant::now(), Duration::from_millis(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut events = futures::stream::repeat(());

        let deadline = TokioInstant::now() + Duration::from_millis(50);
        let mut ticks = 0usize;

        while ticks < 3 && TokioInstant::now() < deadline {
            tokio::select! {
                _ = interval.tick() => {
                    ticks += 1;
                }
                _ = events.next() => {
                    tokio::task::yield_now().await;
                }
            }
        }

        assert!(
            ticks >= 3,
            "background interval ticks were starved; observed {ticks}"
        );
    }

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

    #[test]
    fn command_is_valid_detects_builtin_and_alias() {
        let env = Environment::new();
        {
            let mut writer = env.write();
            writer.alias.insert("ll".to_string(), "ls -al".to_string());
        }

        let mut shell = Shell::new(env.clone());
        let repl = Repl::new(&mut shell);

        assert!(
            repl.command_is_valid("cd"),
            "built-in command should be valid"
        );
        assert!(repl.command_is_valid("ll"), "alias should be valid");
        assert!(
            !repl.command_is_valid("definitely_not_a_command_42"),
            "unknown command should not be valid"
        );

        drop(repl);
    }
}
