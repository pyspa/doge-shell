use crate::builtin;
use crate::completion::{self, Completion};
use crate::dirs;
use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::input::Input;
use crate::parser::{self, Rule, ShellParser};
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
use nix::unistd::{getpid, pipe, setpgid, tcsetpgrp, Pid};
use pest::iterators::Pair;
use pest::Parser;
use std::fs::File;
use std::io::prelude::*;
use std::io::Write;
use std::os::unix::io::FromRawFd;
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
    pub environment: Environment,
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
                    self.input.completion = None;
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

                if let Some(val) = completion::input_completion(
                    &self.input.as_str(),
                    &self.environment.config.borrow().completions,
                    completion_query,
                ) {
                    self.input.insert_str(val.as_str());
                }

                self.start_completion = true;
            }
            (KeyCode::Enter, NONE) => {
                self.input.completion.take();
                self.stop_history_mode();
                print!("\r\n");
                if !self.input.is_empty() {
                    let input = self.input.as_str();
                    self.eval_str(input, false)?;
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
                    let input = self.input.as_str();
                    self.eval_str(input, true)?;
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
            self.print_input(reset_completion);
        } else {
            self.print_prompt();
        }
        Ok(())
    }

    fn print_input(&mut self, reset_completion: bool) {
        let mut stdout = std::io::stdout();

        queue!(stdout, cursor::Hide).ok();
        let input = self.input.as_str();
        let prompt = self.get_prompt().chars().count();

        let fg_color = Color::White;
        let mut comp: Option<String> = None;

        if input.is_empty() || reset_completion {
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

            let mut match_index: Vec<usize> = Vec::new();

            // TODO refactor
            if let Ok(words) = self.input.get_words() {
                for (ref rule, ref span, current) in words {
                    let word = span.as_str();
                    if let Some(_found) = self.environment.lookup(word) {
                        for pos in span.start()..span.end() {
                            // change color
                            match_index.push(pos);
                        }
                    }

                    if !word.is_empty() && current && comp.is_none() {
                        match rule {
                            Rule::argv0 => {
                                if let Some(file) = self.environment.search(word) {
                                    if file.len() >= input.len() {
                                        comp = Some(file[input.len()..].to_string());
                                    }
                                    self.input.completion = Some(file);
                                    break;
                                } else if let Ok(Some(ref dir)) =
                                    completion::path_completion_first(word)
                                {
                                    if dirs::is_dir(dir) {
                                        if dir.len() >= input.len() {
                                            comp = Some(dir[input.len()..].to_string());
                                        }
                                        self.input.completion = Some(dir.clone());
                                        break;
                                    }
                                }
                            }
                            Rule::args => {
                                if let Ok(Some(ref path)) = completion::path_completion_first(word)
                                {
                                    if path.len() >= word.len() {
                                        let part = path[word.len()..].to_string();
                                        comp = Some(path[word.len()..].to_string());
                                        self.input.completion = Some(input + &part);
                                        break;
                                    }
                                } else if !word.starts_with('-') {
                                    if let Some(file) = self.environment.search(word) {
                                        if file.len() >= word.len() {
                                            let part = file[word.len()..].to_string();
                                            comp = Some(file[word.len()..].to_string());
                                            self.input.completion = Some(input + &part);
                                            break;
                                        }
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

    pub fn print_error(&self, msg: String) {
        // unknown command, etc
        eprint!("\r{}\r\n", msg);
        std::io::stderr().flush().ok();
    }

    pub fn print_stdout(&self, msg: String) {
        // unknown command, etc
        print!("\r{}\r\n", msg);
        std::io::stdout().flush().ok();
    }

    fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.cmd_history {
            history.search_word = None;
            history.reset_index();
        }
    }

    fn eval_str(&mut self, input: String, background: bool) -> Result<()> {
        if let Some(ref mut history) = self.cmd_history {
            history.add(&input);
            history.reset_index();
        }

        let tmode = tcgetattr(0).expect("failed tcgetattr");
        let mut ctx = Context::new(self.pid, self.pgid, tmode, true);

        let jobs = self.get_jobs(input)?;

        for mut job in jobs {
            disable_raw_mode().ok();
            if background {
                // all job run background
                job.foreground = false;
            }
            if let process::ProcessState::Completed(exit) = job.launch(&mut ctx, self)? {
                if exit != 0 {
                    // TODO check
                    debug!("job exit code {:?}", exit);
                }
            } else {
                // Stop next job
            }
            enable_raw_mode().ok();
        }

        Ok(())
    }

    fn get_jobs(&mut self, input: String) -> Result<Vec<Job>> {
        // TODO tests

        let input = parser::expand_alias(input, &self.environment.config.borrow().alias)?;

        let mut pairs = ShellParser::parse(Rule::commands, &input).map_err(|e| anyhow!(e))?;

        if let Some(pair) = pairs.next() {
            self.parse_commands(pair)
        } else {
            Ok(Vec::new())
        }
    }

    fn get_argv(&mut self, pair: Pair<Rule>) -> Result<Vec<(String, Option<Vec<process::Job>>)>> {
        let mut argv: Vec<(String, Option<Vec<process::Job>>)> = vec![];

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::argv0 => {
                    for inner_pair in inner_pair.into_inner() {
                        // span
                        for inner_pair in inner_pair.into_inner() {
                            match inner_pair.as_rule() {
                                Rule::subshell => {
                                    for inner_pair in inner_pair.into_inner() {
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        let res = self.parse_commands(inner_pair)?;
                                        argv.push((cmd_str, Some(res)));
                                    }
                                }
                                _ => {
                                    if let Some(arg) = parser::get_string(inner_pair) {
                                        argv.push((arg, None));
                                    }
                                }
                            }
                        }
                    }
                }
                Rule::args => {
                    for inner_pair in inner_pair.into_inner() {
                        // span
                        for inner_pair in inner_pair.into_inner() {
                            match inner_pair.as_rule() {
                                Rule::subshell => {
                                    for inner_pair in inner_pair.into_inner() {
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        let res = self.parse_commands(inner_pair)?;
                                        argv.push((cmd_str, Some(res)));
                                    }
                                }
                                _ => {
                                    if let Some(arg) = parser::get_string(inner_pair) {
                                        argv.push((arg, None));
                                    }
                                }
                            }
                        }
                    }
                }
                Rule::simple_command => {
                    let mut res = self.get_argv(inner_pair)?;
                    argv.append(&mut res);
                }
                _ => {
                    warn!("missing {:?}", inner_pair.as_rule());
                }
            }
        }
        Ok(argv)
    }

    fn parse_commands(&mut self, pair: Pair<Rule>) -> Result<Vec<Job>> {
        let mut jobs: Vec<Job> = Vec::new();
        if let Rule::commands = pair.as_rule() {
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::command => self.parse_jobs(pair, &mut jobs)?,
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

        Ok(jobs)
    }

    fn launch_subshell(&mut self, jobs: Vec<Job>) -> Result<String> {
        let tmode = tcgetattr(0).expect("failed tcgetattr");
        let mut ctx = Context::new(self.pid, self.pgid, tmode, true);
        ctx.foreground = false;
        let (pout, pin) = pipe().context("failed pipe")?;
        ctx.outfile = pin;

        for mut job in jobs {
            disable_raw_mode().ok();
            if let Ok(process::ProcessState::Completed(exit)) = job.launch(&mut ctx, self) {
                if exit != 0 {
                    // TODO check
                    debug!("job exit code {:?}", exit);
                }
            } else {
                // Stop next job
            }
            enable_raw_mode().ok();
            // debug!("{:?}", job.cmd);
        }
        let mut raw_stdout = Vec::new();
        unsafe { File::from_raw_fd(pout).read_to_end(&mut raw_stdout).ok() };

        let output = std::str::from_utf8(&raw_stdout)
            .map_err(|err| {
                // TODO
                eprintln!("binary in variable/expansion is not supported");
                err
            })?
            .trim_end_matches('\n')
            .to_owned();
        Ok(output)
    }

    fn parse_command(&mut self, job: &mut Job, pair: Pair<Rule>, foreground: bool) -> Result<bool> {
        let parsed = self.get_argv(pair)?;
        if parsed.is_empty() {
            return Ok(false);
        }

        let mut argv: Vec<String> = Vec::new();
        for (str, jobs) in parsed {
            if let Some(jobs) = jobs {
                let output = self.launch_subshell(jobs)?;
                output.lines().for_each(|x| argv.push(x.to_owned()));
            } else {
                argv.push(str);
            }
        }

        let cmd = argv[0].as_str();
        let mut result = true;

        if let Some(cmd_fn) = builtin::get_command(cmd) {
            // TODO check return lock
            let builtin = process::BuiltinProcess::new(cmd_fn, argv);
            job.set_process(JobProcess::Builtin(builtin));
        } else if self.environment.wasm_engine.modules.get(cmd).is_some() {
            let wasm = process::WasmProcess::new(cmd.to_string(), argv);
            job.set_process(JobProcess::Wasm(wasm));
        } else if let Some(cmd) = self.environment.lookup(cmd) {
            let process = process::Process::new(cmd, argv);
            job.set_process(JobProcess::Command(process));
            job.foreground = foreground;
        } else if dirs::is_dir(cmd) {
            if let Some(cmd_fn) = builtin::get_command("cd") {
                let builtin =
                    process::BuiltinProcess::new(cmd_fn, vec!["cd".to_string(), cmd.to_string()]);
                job.set_process(JobProcess::Builtin(builtin));
            }
        } else {
            result = false;
            self.print_error(format!("unknown command: {}", cmd));
        }
        Ok(result)
    }

    fn parse_jobs(&mut self, pair: Pair<Rule>, jobs: &mut Vec<Job>) -> Result<()> {
        let job_str = pair.as_str().to_string();

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let mut job = Job::new(job_str.clone());
                    self.parse_command(&mut job, inner_pair, true)?;
                    if job.process.is_some() {
                        jobs.push(job);
                    }
                }
                Rule::simple_command_bg => {
                    // background job
                    let mut job = Job::new(inner_pair.as_str().to_string());
                    for bg_pair in inner_pair.into_inner() {
                        if let Rule::simple_command = bg_pair.as_rule() {
                            self.parse_command(&mut job, bg_pair, false)?;
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
                                self.parse_command(job, inner_pair, true)?;
                            } else if let Rule::simple_command_bg = inner_pair.as_rule() {
                                self.parse_command(job, inner_pair, false)?;
                            } else {
                                // TODO check?
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn run_wasm(&mut self, name: &str, args: Vec<String>) -> Result<()> {
        // TODO support ctx
        self.environment.wasm_engine.call(name, args.as_ref())
    }

    pub fn exit(&mut self) {
        self.input.clear();
        self.exited = Some(ExitStatus::ExitedWith(0));
    }
}
