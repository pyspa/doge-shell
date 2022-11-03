use crate::builtin;
use crate::completion::{self, Completion};
use crate::config::Config;
use crate::dirs;
use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::input::Input;
use crate::parser::{expand_alias, get_argv, Rule, ShellParser};
use crate::process::{self, wait_any_job, Context, ExitStatus, Job, JobProcess, WaitJob};
use crate::prompt::print_preprompt;
use anyhow::Context as _;
use anyhow::{anyhow, Result};
use crossterm::cursor;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, Stylize};
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode, Clear, ClearType};
use crossterm::{execute, queue};
use futures::{future::FutureExt, select, StreamExt};
use futures_timer::Delay;
use libc::{c_int, STDIN_FILENO};
use log::{debug, warn};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::{tcgetattr, Termios};
use nix::unistd::{getpid, setpgid, tcsetpgrp, Pid};
use pest::iterators::Pair;
use pest::Parser;
use std::io::Write;
use std::time::Duration;

pub const APP_NAME: &str = "dsh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

#[derive(Eq, PartialEq)]
pub enum ShellEvent {
    Input(Event),
    ScreenResized,
}

#[derive(Debug)]
pub struct Shell {
    environment: Environment,
    input: Input,
    columns: usize,
    lines: usize,
    exited: Option<ExitStatus>,
    pid: Pid,
    pgid: Pid,
    pub cmd_history: Option<FrecencyHistory>,
    pub path_history: Option<FrecencyHistory>,
    tmode: Option<Termios>,
    history_search: Option<String>,
    start_completion: bool,
    config: Config,
    pub wait_jobs: Vec<WaitJob>,
    completion: Completion,
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.save_history();
    }
}

impl Shell {
    pub fn new(environment: Environment) -> Self {
        let pid = getpid();
        let pgid = pid;

        let cmd_history = FrecencyHistory::from_file("dsh_cmd_history").unwrap();
        let path_history = FrecencyHistory::from_file("dsh_path_history").unwrap();
        let config = Config::from_file("config.toml");
        Shell {
            environment,
            input: Input::new(),
            columns: 0,
            lines: 0,
            exited: None::<ExitStatus>,
            pid,
            pgid,
            cmd_history: Some(cmd_history),
            path_history: Some(path_history),
            tmode: None,
            history_search: None,
            start_completion: false,
            config,
            wait_jobs: Vec::new(),
            completion: Completion::new(),
        }
    }

    fn setup(&mut self) {
        let screen_size = terminal::size().unwrap();
        self.columns = screen_size.0 as usize;
        self.lines = screen_size.1 as usize;
        enable_raw_mode().ok();
    }

    fn get_prompt(&self) -> &str {
        //"$"
        "🐕 < "
    }

    fn set_signals(&mut self) {
        let action = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action).expect("failed sigaction");
            sigaction(Signal::SIGQUIT, &action).expect("failed sigaction");
            sigaction(Signal::SIGTSTP, &action).expect("failed sigaction");
            sigaction(Signal::SIGTTIN, &action).expect("failed sigaction");
            sigaction(Signal::SIGTTOU, &action).expect("failed sigaction");
        }
    }

    fn check_background_jobs(&mut self) {
        if let Some((pid, _state)) = wait_any_job(true) {
            if let Some(index) = self.wait_jobs.iter().position(|job| job.pid == pid) {
                if let Some(job) = self.wait_jobs.get(index) {
                    // TODO fix message format
                    print!("\r\n[{:?}] done '{}' \r\n\r", job.job_id, job.cmd);
                    self.wait_jobs.remove(index);
                    self.print_prompt();
                }
            }
        }
    }

    fn save_history(&mut self) {
        if let Some(ref mut history) = self.cmd_history {
            let _ = history.save();
        }
        if let Some(ref mut history) = self.path_history {
            let _ = history.save();
        }
    }

    pub async fn run_interactive(&mut self) {
        let mut reader = EventStream::new();

        self.setup();
        self.set_signals();

        debug!("shell setpgid pid:{:?} pgid:{:?}", self.pid, self.pgid);
        let _ = setpgid(self.pgid, self.pgid).context("failed setpgid");
        let _ = tcsetpgrp(SHELL_TERMINAL, self.pgid).context("failed tcsetpgrp");
        self.tmode = Some(tcgetattr(SHELL_TERMINAL).expect("failed cgetattr"));

        // start repl loop
        self.print_prompt();

        loop {
            let mut save_history_delay = Delay::new(Duration::from_millis(10_000)).fuse();
            let mut check_background_delay = Delay::new(Duration::from_millis(1_000)).fuse();
            let mut event = reader.next().fuse();

            select! {
                _ = check_background_delay => {
                    self.check_background_jobs();
                },
                _ = save_history_delay => {
                    self.save_history();
                },
                maybe_event = event => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            if let Err(err) = self.handle_event(ShellEvent::Input(event)){
                                self.print_error(format!("Error: {:?}\r",err));
                                break;
                            }
                        }
                        Some(Err(err)) => {
                            self.print_error(format!("Error: {:?}\r",err));
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
            if let Some(_status) = self.exited {
                debug!("exited");
                break;
            }
        }
    }

    fn handle_event(&mut self, ev: ShellEvent) -> Result<()> {
        match ev {
            ShellEvent::Input(input) => {
                if let Event::Key(key) = input {
                    self.handle_key_event(&key)?
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

    fn set_completions(&mut self) {
        if let Some(ref mut history) = self.cmd_history {
            let comps = if self.input.is_empty() {
                history.sorted(&crate::frecency::SortMethod::Recent)
            } else {
                history.sort_by_match(&self.input.as_str())
            };
            self.completion.set_completions(&self.input.as_str(), comps);
        }
    }

    fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<()> {
        let mut redraw = true;
        let mut backspace = false;
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
                    self.input.move_by(-1);
                    let mut stdout = std::io::stdout();
                    queue!(stdout, cursor::MoveLeft(1)).ok();
                    stdout.flush().ok();
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
                    self.input.move_by(1);
                    let mut stdout = std::io::stdout();
                    queue!(stdout, cursor::MoveRight(1)).ok();
                    stdout.flush().ok();
                    self.completion.clear();
                }
                return Ok(());
            }
            (KeyCode::Char(ch), NONE) => {
                self.input.insert(ch);
                if self.completion.is_changed(&self.input.as_str()) {
                    self.completion.clear();
                }
            }
            (KeyCode::Char(ch), SHIFT) => {
                self.input.insert(ch);
                if self.completion.is_changed(&self.input.as_str()) {
                    self.completion.clear();
                }
            }
            (KeyCode::Backspace, NONE) => {
                backspace = true;
                self.input.backspace();
                self.completion.clear();
                self.input.match_index = None;
            }
            (KeyCode::Tab, NONE) | (KeyCode::BackTab, NONE) => {
                for compl in &self.config.completions {
                    let cmd_str = format!("{} ", compl.target);
                    if self.input.as_str() == cmd_str {
                        let res = if compl.completion_cmd == "git_branch" {
                            completion::git_branch(None)
                        } else {
                            completion::completion_from_cmd(compl.completion_cmd.to_string(), None)
                        };

                        if let Some(val) = res {
                            self.input.insert_str(val.as_str());
                        }

                        break;
                    }
                }
                self.start_completion = true;
            }
            (KeyCode::Enter, NONE) => {
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    let input = self.input.as_str();
                    self.eval_str(input)?;
                    self.input.clear();
                }
                redraw = false;
                // self.print_prompt();
            }
            (KeyCode::Char('a'), CTRL) => {
                self.input.move_to_begin();
            }
            (KeyCode::Char('c'), CTRL) => {
                execute!(std::io::stdout(), Print("\r\n")).ok();
                self.print_prompt();
                self.input.clear();
            }
            (KeyCode::Char('l'), CTRL) => {
                execute!(
                    std::io::stdout(),
                    Clear(ClearType::All),
                    cursor::MoveTo(0, 0)
                )
                .ok();
                self.print_prompt();
                self.input.clear();
            }
            (KeyCode::Char('d'), CTRL) => {
                self.exit();
            }

            _ => {
                warn!("unsupported key event: {:?}", ev);
            }
        }

        if redraw {
            self.print_input(backspace);
        } else {
            self.print_prompt();
        }
        Ok(())
    }

    fn print_input(&mut self, backspace: bool) {
        let mut stdout = std::io::stdout();

        queue!(stdout, cursor::Hide).ok();
        let input = self.input.as_str();
        let prompt = self.get_prompt().chars().count();

        let mut fg_color = Color::White;
        let mut comp: Option<String> = None;

        if input.is_empty() || backspace {
            self.input.completion = None
        } else {
            // TODO refactor

            if let Some(ref mut history) = self.cmd_history {
                if let Some(hist) = history.search_first(&input) {
                    self.input.completion = Some(hist.clone());
                    if hist.len() >= input.len() {
                        comp = Some(hist[input.len()..].to_string());
                    }
                }
            }
            if comp.is_none() {
                if let Ok(Some((rule, ref span))) = self.input.get_cursor_word() {
                    let word = span.as_str();
                    match rule {
                        Rule::argv0 => {
                            // command
                            if let Some(_found) = self.environment.lookup(word) {
                                fg_color = Color::Blue;
                            } else if let Some(file) = self.environment.search(word) {
                                if file.len() >= input.len() {
                                    comp = Some(file[input.len()..].to_string());
                                }
                                self.input.completion = Some(file);
                            } else if let Ok(Some(ref dir)) =
                                completion::path_completion_first(word)
                            {
                                if dirs::is_dir(dir) {
                                    if dir.len() >= input.len() {
                                        comp = Some(dir[input.len()..].to_string());
                                    }
                                    self.input.completion = Some(dir.clone());
                                }
                            }
                        }
                        Rule::args => {
                            if word.len() > 1 {
                                // if let Some(file) = self.environment.search(&word) {
                                //     if file.len() >= word.len() {
                                //         let part = file[word.len()..].to_string();
                                //         comp = Some(file[word.len()..].to_string());
                                //         self.input.completion = Some(input.to_string() + &part);
                                //     }
                                // } else
                                if let Ok(Some(ref path)) = completion::path_completion_first(word)
                                {
                                    if path.len() >= word.len() {
                                        let part = path[word.len()..].to_string();
                                        comp = Some(path[word.len()..].to_string());
                                        self.input.completion = Some(input + &part);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        queue!(
            stdout,
            Print("\r"),
            cursor::MoveRight((prompt + 1) as u16),
            Clear(ClearType::UntilNewLine),
        )
        .ok();

        self.input.print(fg_color);

        self.move_cursor_input_end();

        if let Some(comp) = comp {
            print!("{}", comp.dark_grey());
            self.move_cursor_input_end();
        }
        queue!(stdout, cursor::Show).ok();

        stdout.flush().ok();
    }

    fn move_cursor_input_end(&self) {
        let mut stdout = std::io::stdout();
        let prompt_size = self.get_prompt().chars().count();
        queue!(
            stdout,
            ResetColor,
            cursor::MoveToColumn((prompt_size + self.input.cursor() + 1) as u16),
        )
        .ok();
    }

    fn move_cursor(&self, len: usize) {
        let mut stdout = std::io::stdout();
        let prompt_size = self.get_prompt().chars().count();
        queue!(
            stdout,
            ResetColor,
            cursor::MoveToColumn((prompt_size + len + 1) as u16),
        )
        .ok();
    }

    fn print_prompt(&mut self) {
        let prompt = self.get_prompt();
        print_preprompt();
        print!("\r{}", prompt);
        std::io::stdout().flush().ok();
    }

    fn print_error(&self, msg: String) {
        // unknown command, etc
        eprint!("\r{}\r\n", msg);
        std::io::stderr().flush().ok();
    }

    fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.cmd_history {
            history.search_word = None;
            history.reset_index();
        }
    }

    fn eval_str(&mut self, input: String) -> Result<()> {
        if let Some(ref mut history) = self.cmd_history {
            history.add(&input);
            history.reset_index();
        }

        let tmode = tcgetattr(0).expect("failed tcgetattr");
        let mut ctx = Context::new(self.pid, self.pgid, tmode, true);

        let jobs = self.get_jobs(input)?;

        for mut job in jobs {
            disable_raw_mode().ok();
            job.launch(&mut ctx, self)?;
            enable_raw_mode().ok();
        }

        Ok(())
    }

    fn get_jobs(&self, input: String) -> Result<Vec<Job>> {
        // TODO tests

        let input = expand_alias(input, &self.config.alias)?;

        let pairs = ShellParser::parse(Rule::commands, &input).map_err(|e| anyhow!(e))?;

        let mut jobs: Vec<Job> = Vec::new();

        for pair in pairs {
            if let Rule::commands = pair.as_rule() {
                for pair in pair.into_inner() {
                    match pair.as_rule() {
                        Rule::command => {
                            if let Some(ref mut parsed) = self.parse_jobs(pair) {
                                jobs.append(parsed);
                            }
                        }
                        Rule::command_list_sep => {
                            // TODO keep separator type
                            // simple list
                        }
                        _ => {
                            debug!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                        }
                    }
                }
            }
        }

        Ok(jobs)
    }

    fn parse_command(&self, job: &mut Job, pair: Pair<Rule>, foreground: bool) -> bool {
        let argv = get_argv(pair);
        if argv.is_empty() {
            return false;
        }
        let cmd = argv[0].as_str();
        let mut result = true;

        if let Some(cmd_fn) = builtin::BUILTIN_COMMAND.get(cmd) {
            let builtin = process::BuiltinProcess::new(*cmd_fn, argv);
            job.set_process(JobProcess::Builtin(builtin));
        } else if let Some(cmd) = self.environment.lookup(cmd) {
            let process = process::Process::new(cmd, argv);
            job.set_process(JobProcess::Command(process));
            job.foreground = foreground;
        } else if dirs::is_dir(cmd) {
            if let Some(cmd_fn) = builtin::BUILTIN_COMMAND.get("cd") {
                let builtin =
                    process::BuiltinProcess::new(*cmd_fn, vec!["cd".to_string(), cmd.to_string()]);
                job.set_process(JobProcess::Builtin(builtin));
            }
        } else {
            result = false;
            self.print_error(format!("unknown command: {}", cmd));
        }
        result
    }

    fn parse_jobs(&self, pair: Pair<Rule>) -> Option<Vec<Job>> {
        let job_str = pair.as_str().to_string();
        debug!("@ {:?} {:?}", pair.as_rule(), pair.as_str());
        let mut jobs: Vec<Job> = Vec::new();

        for inner_pair in pair.into_inner() {
            debug!("{:?} {:?}", inner_pair.as_rule(), inner_pair.as_str());
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let mut job = Job::new(job_str.clone());
                    self.parse_command(&mut job, inner_pair, true);
                    if job.process.is_some() {
                        jobs.push(job);
                    }
                }
                Rule::simple_command_bg => {
                    // background job
                    let mut job = Job::new(inner_pair.as_str().to_string());
                    for bg_pair in inner_pair.into_inner() {
                        if let Rule::simple_command = bg_pair.as_rule() {
                            self.parse_command(&mut job, bg_pair, false);
                            if job.process.is_some() {
                                jobs.push(job);
                            }
                            break;
                        }
                    }
                }
                Rule::pipe_command => {
                    if let Some(job) = jobs.last_mut() {
                        for inner_pair in inner_pair.into_inner() {
                            let _cmd = inner_pair.as_str();
                            if let Rule::simple_command = inner_pair.as_rule() {
                                self.parse_command(job, inner_pair, true);
                            } else if let Rule::simple_command_bg = inner_pair.as_rule() {
                                self.parse_command(job, inner_pair, false);
                            } else {
                                // TODO check?
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if jobs.is_empty() {
            None
        } else {
            Some(jobs)
        }
    }

    pub fn exit(&mut self) {
        self.input.clear();
        self.exited = Some(ExitStatus::ExitedWith(0));
    }
}
