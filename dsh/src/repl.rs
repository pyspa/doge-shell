use crate::completion::{self, Completion};
use crate::dirs;
use crate::input::Input;
use crate::parser::Rule;
use crate::prompt::Prompt;
use crate::shell::{Shell, SHELL_TERMINAL};
use anyhow::Context as _;
use anyhow::Result;
use crossterm::cursor;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, Stylize};
use crossterm::terminal::{self, enable_raw_mode, Clear, ClearType};
use crossterm::{execute, queue};
use dsh_types::Context;
use futures::{future::FutureExt, select, StreamExt};
use futures_timer::Delay;
use nix::sys::termios::{tcgetattr, Termios};
use nix::unistd::tcsetpgrp;
use parking_lot::RwLock;
use std::io::{StdoutLock, Write};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

#[derive(Eq, PartialEq)]
pub enum ShellEvent {
    Input(Event),
    ScreenResized,
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
}

impl<'a> Drop for Repl<'a> {
    fn drop(&mut self) {
        self.save_history();
    }
}

impl<'a> Repl<'a> {
    pub fn new(shell: &'a mut Shell) -> Self {
        let current = std::env::current_dir().expect("failed get current dir");
        let prompt = Prompt::new(current, "🐕 < ".to_string());

        let prompt = Arc::new(RwLock::new(prompt));
        shell
            .environment
            .write()
            .chpwd_hooks
            .push(Box::new(Arc::clone(&prompt)));

        Repl {
            shell,
            input: Input::new(),
            columns: 0,
            lines: 0,
            tmode: None,
            history_search: None,
            start_completion: false,
            completion: Completion::new(),
            prompt,
        }
    }

    fn setup(&mut self) {
        let screen_size = terminal::size().unwrap();
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
            let mut history = history.lock().unwrap();
            history.save().expect("failed save cmd history");
        }
        if let Some(ref mut history) = self.shell.path_history {
            let mut history = history.lock().unwrap();
            history.save().expect("failed save path history");
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
        let prompt = self.prompt.write();
        let prompt_mark = &prompt.mark;
        prompt.print_preprompt(out);
        out.write(b"\r").ok();
        out.write(prompt_mark.as_bytes()).ok();
        out.flush().ok();
    }

    fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.shell.cmd_history {
            let mut history = history.lock().unwrap();
            history.search_word = None;
            history.reset_index();
        }
    }

    fn set_completions(&mut self) {
        if let Some(ref mut history) = self.shell.cmd_history {
            let history = history.lock().unwrap();
            let comps = if self.input.is_empty() {
                history.sorted(&dsh_frecency::SortMethod::Recent)
            } else {
                history.sort_by_match(self.input.as_str())
            };
            self.completion.set_completions(self.input.as_str(), comps);
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
                let screen_size = terminal::size().unwrap();
                self.columns = screen_size.0 as usize;
                self.lines = screen_size.1 as usize;
                Ok(())
            }
        }
    }

    fn get_completion_from_history(&mut self, input: &str) -> Option<String> {
        if let Some(ref mut history) = self.shell.cmd_history {
            let history = history.lock().unwrap();
            if let Some(entry) = history.search_prefix(input) {
                self.input.completion = Some(entry.clone());
                if entry.len() >= input.len() {
                    return Some(entry[input.len()..].to_string());
                }
            }
        }
        None
    }

    pub fn print_input(&mut self, out: &mut StdoutLock<'static>, reset_completion: bool) {
        queue!(out, cursor::Hide).ok();
        let input = self.input.to_string();
        let prompt_count = self.prompt.write().mark.chars().count();

        let fg_color = Color::White;
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
                                        self.input.completion = Some(input + &part);
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

        queue!(
            out,
            Print("\r"),
            cursor::MoveRight((prompt_count + 1) as u16),
            Clear(ClearType::UntilNewLine),
        )
        .ok();

        self.input.print(out, fg_color);

        self.move_cursor_input_end(out);

        if let Some(completion) = completion {
            out.write_fmt(format_args!("{}", completion.dark_grey()))
                .ok();
            self.move_cursor_input_end(out);
        }
        queue!(out, cursor::Show).ok();
    }

    async fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<()> {
        let mut redraw = true;
        let mut reset_completion = false;

        match (ev.code, ev.modifiers) {
            // history
            (KeyCode::Up, NONE) => {
                if self.completion.completion_mode() {
                    if let Some(item) = self.completion.backward() {
                        self.input
                            .reset_with_match_index(item.item, item.match_index);
                    } else {
                    }
                } else {
                    self.set_completions();
                    if let Some(item) = self.completion.backward() {
                        self.input
                            .reset_with_match_index(item.item, item.match_index);
                    } else {
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
                if let Some(comp) = &self.input.completion.take() {
                    self.input.reset(comp.to_string());
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
                if let Some(val) = completion::input_completion(&self.input, self, completion_query)
                {
                    if let Some(q) = completion_query {
                        self.input.backspacen(q.len());
                    }
                    self.input.insert_str(val.as_str());
                }

                self.start_completion = true;
            }
            (KeyCode::Enter, NONE) => {
                self.input.completion.take();
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    self.completion.clear();
                    let shell_tmode = tcgetattr(0).expect("failed tcgetattr");
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
                    let shell_tmode = tcgetattr(0).expect("failed tcgetattr");
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
            (KeyCode::Char('c'), CTRL) => {
                let mut out = std::io::stdout().lock();
                execute!(out, Print("\r\n")).ok();
                self.print_prompt(&mut out);
                self.input.clear();
                return Ok(());
            }
            (KeyCode::Char('l'), CTRL) => {
                let mut out = std::io::stdout().lock();
                execute!(out, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
                self.print_prompt(&mut out);
                self.input.clear();
                println!("zap");
                return Ok(());
            }
            (KeyCode::Char('d'), CTRL) => {
                self.shell.exit();
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
            self.print_input(&mut out, reset_completion);
        } else {
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
        self.tmode = Some(tcgetattr(SHELL_TERMINAL).expect("failed cgetattr"));
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
                        async_std::task::spawn(async move{
                            let mut history = history.lock().unwrap();
                            history.save().expect("failed save path history");
                        });
                    }
                    if let Some(ref mut history) = self.shell.cmd_history {
                        let history = history.clone();
                        async_std::task::spawn(async move{
                            let mut history = history.lock().unwrap();
                            history.save().expect("failed save cmd history");
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
            let mut history = history.lock().unwrap();
            let histories = history.sorted(&dsh_frecency::SortMethod::Frecent);
            if let Some(val) = completion::select_item(
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
    }
}
