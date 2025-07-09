use crate::completion::{self, Completion};
use crate::dirs;
use crate::input::{Input, InputConfig};
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

/// Ctrl+C二回押し検出用の状態管理
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

    /// Ctrl+Cが押された時の処理。二回目の場合はtrueを返す
    fn on_ctrl_c_pressed(&mut self) -> bool {
        let now = Instant::now();

        match self.first_press_time {
            None => {
                // 初回押下
                self.first_press_time = Some(now);
                self.press_count = 1;
                false
            }
            Some(first_time) => {
                if now.duration_since(first_time) <= Duration::from_secs(3) {
                    // 3秒以内の二回目押下
                    self.press_count = 2;
                    true
                } else {
                    // 3秒を超えているので初回扱い
                    self.first_press_time = Some(now);
                    self.press_count = 1;
                    false
                }
            }
        }
    }

    /// 状態をリセット
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
    prompt: Arc<RwLock<Prompt>>,
    ctrl_c_state: CtrlCState,
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
            prompt,
            ctrl_c_state: CtrlCState::new(),
        }
    }

    fn setup(&mut self) {
        let screen_size = terminal::size().unwrap_or_else(|e| {
            warn!("Failed to get terminal size: {}, using default 80x24", e);
            (80, 24)
        });
        self.columns = screen_size.0 as usize;
        self.lines = screen_size.1 as usize;
        enable_raw_mode().ok();
    }

    async fn check_background_jobs(&mut self, output: bool) -> Result<()> {
        let mut out = std::io::stdout().lock();
        let jobs = self.shell.check_job_state().await?;
        let exists = !jobs.is_empty();

        for mut job in jobs {
            if !job.foreground {
                job.check_background_all_output().await?;
            }
            if output {
                out.write_fmt(format_args!(
                    "\rdsh: job {} '{}' {}\n",
                    job.job_id, job.cmd, job.state
                ))?;
            }
        }

        if exists && output {
            // out.write(b"\r\n").ok();
            self.print_prompt(&mut out);
            out.flush()?;
        }
        Ok(())
    }

    fn save_history(&mut self) {
        if let Some(ref mut history) = self.shell.cmd_history {
            match history.lock() {
                Ok(mut history) => {
                    if let Err(e) = history.save() {
                        warn!("Failed to save command history: {}", e);
                    }
                }
                Err(e) => {
                    warn!("Failed to acquire command history lock for saving: {}", e);
                }
            }
        }
        if let Some(ref mut history) = self.shell.path_history {
            match history.lock() {
                Ok(mut history) => {
                    if let Err(e) = history.save() {
                        warn!("Failed to save path history: {}", e);
                    }
                }
                Err(e) => {
                    warn!("Failed to acquire path history lock for saving: {}", e);
                }
            }
        }
    }

    fn move_cursor_input_end(&self, out: &mut StdoutLock<'static>) {
        let prompt_size = self.prompt.read().mark.chars().count();
        queue!(
            out,
            ResetColor,
            cursor::MoveToColumn((prompt_size + self.input.cursor() + 1) as u16),
        )
        .ok();
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
        let prompt_count = self.prompt.write().mark.chars().count();
        debug!("Current input: '{}', prompt_count: {}", input, prompt_count);

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
                                } else if let Ok(Some(ref dir)) =
                                    completion::path_completion_prefix(word)
                                {
                                    if dirs::is_dir(dir) {
                                        if dir.len() >= input.len() {
                                            completion = Some(dir[input.len()..].to_string());
                                        }
                                        self.input.completion = Some(dir.clone());
                                        break;
                                    }
                                }
                            }
                            Rule::args => {
                                if let Ok(Some(ref path)) = completion::path_completion_prefix(word)
                                {
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
        let mut redraw = true;
        let mut reset_completion = false;

        // Ctrl+C以外のキー入力時はCtrl+C状態をリセット
        if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
            self.ctrl_c_state.reset();
        }

        match (ev.code, ev.modifiers) {
            // history
            (KeyCode::Up, NONE) => {
                if self.completion.completion_mode() {
                    if let Some(item) = self.completion.backward() {
                        self.input
                            .reset_with_match_index(item.item, item.match_index);
                    }
                } else {
                    self.set_completions();
                    if let Some(item) = self.completion.backward() {
                        self.input
                            .reset_with_match_index(item.item, item.match_index);
                    }
                }
            }
            // history
            (KeyCode::Down, NONE) => {
                if self.completion.completion_mode() {
                    if let Some(item) = self.completion.forward() {
                        self.input
                            .reset_with_match_index(item.item, item.match_index);
                    }
                }
            }
            (KeyCode::Left, NONE) => {
                if self.input.cursor() > 0 {
                    let mut out = std::io::stdout().lock();
                    self.input.completion = None;
                    self.input.move_by(-1);
                    queue!(out, cursor::MoveLeft(1)).ok();
                    out.flush().ok();
                    self.completion.clear();
                }
                return Ok(());
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
                    let mut out = std::io::stdout().lock();
                    self.input.move_by(1);
                    queue!(out, cursor::MoveRight(1)).ok();
                    out.flush().ok();
                    self.completion.clear();
                }
                return Ok(());
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

                if let Some(val) = completion::input_completion(
                    &self.input,
                    self,
                    completion_query,
                    prompt_text,
                    input_text,
                ) {
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
                            eprintln!("{:?}", err);
                        }
                    }
                    self.input.clear();
                }
                redraw = false;
                // self.print_prompt();
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
                        eprintln!("{:?}", err)
                    }
                    self.input.clear();
                }
                redraw = false;
                // self.print_prompt();
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
                    // 二回目のCtrl+C - シェルを終了
                    execute!(out, Print("\r\nExiting shell...\r\n")).ok();
                    return Err(anyhow::anyhow!("Shell terminated by double Ctrl+C"));
                } else {
                    // 初回のCtrl+C - プロンプトリセット + メッセージ表示
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

        let mut out = std::io::stdout().lock();
        if redraw {
            debug!("Redrawing input, reset_completion: {}", reset_completion);
            self.print_input(&mut out, reset_completion);
        } else {
            debug!("Printing prompt only");
            self.print_prompt(&mut out);
        }
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
        let mut out = std::io::stdout().lock();

        // start repl loop
        self.print_prompt(&mut out);
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
                                self.shell.print_error(format!("Error: {:?}\r",err));
                                break;
                            }
                        }
                        Some(Err(err)) => {
                            self.shell.print_error(format!("Error: {:?}\r",err));
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
            if let Some(_status) = self.shell.exited {
                debug!("exited");
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

        // 初回押下は false を返す
        assert!(!state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 1);
        assert!(state.first_press_time.is_some());
    }

    #[test]
    fn test_ctrl_c_state_double_press_within_timeout() {
        let mut state = CtrlCState::new();

        // 初回押下
        assert!(!state.on_ctrl_c_pressed());

        // 短時間後の二回目押下
        thread::sleep(std::time::Duration::from_millis(100));
        assert!(state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 2);
    }

    #[test]
    fn test_ctrl_c_state_double_press_after_timeout() {
        let mut state = CtrlCState::new();

        // 初回押下
        assert!(!state.on_ctrl_c_pressed());

        // 3秒以上後の押下（新しい初回扱い）
        thread::sleep(std::time::Duration::from_secs(4));
        assert!(!state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 1);
    }

    #[test]
    fn test_ctrl_c_state_reset() {
        let mut state = CtrlCState::new();

        // 初回押下
        assert!(!state.on_ctrl_c_pressed());

        // リセット
        state.reset();
        assert_eq!(state.press_count, 0);
        assert!(state.first_press_time.is_none());

        // リセット後の押下は初回扱い
        assert!(!state.on_ctrl_c_pressed());
        assert_eq!(state.press_count, 1);
    }
}
