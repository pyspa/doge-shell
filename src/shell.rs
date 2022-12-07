use crate::builtin;
use crate::dirs;
use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::parser::{self, Rule, ShellParser};
use crate::process::{self, Context, ExitStatus, Job, JobProcess, WaitJob};
use anyhow::Context as _;
use anyhow::{anyhow, bail, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use libc::{c_int, STDIN_FILENO};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::tcgetattr;
use nix::unistd::{getpid, pipe, Pid};
use pest::iterators::Pair;
use pest::Parser;
use std::fs::File;
use std::io::prelude::*;
use std::io::Write;
use std::os::unix::io::FromRawFd;
use std::process::ExitCode;
use tracing::{debug, warn};

pub const APP_NAME: &str = "dsh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;

#[derive(Debug)]
pub struct Shell {
    pub environment: Environment,
    pub exited: Option<ExitStatus>,
    pub pid: Pid,
    pub pgid: Pid,
    pub cmd_history: Option<FrecencyHistory>,
    pub path_history: Option<FrecencyHistory>,
    pub wait_jobs: Vec<WaitJob>,
}

impl Drop for Shell {
    fn drop(&mut self) {
        disable_raw_mode();
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
            exited: None::<ExitStatus>,
            pid,
            pgid,
            cmd_history: Some(cmd_history),
            path_history: Some(path_history),
            wait_jobs: Vec::new(),
        }
    }

    pub fn set_signals(&mut self) {
        let action = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action).expect("failed sigaction");
            sigaction(Signal::SIGQUIT, &action).expect("failed sigaction");
            sigaction(Signal::SIGTSTP, &action).expect("failed sigaction");
            sigaction(Signal::SIGTTIN, &action).expect("failed sigaction");
            sigaction(Signal::SIGTTOU, &action).expect("failed sigaction");
        }
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

    pub fn eval_str(&mut self, input: String, background: bool) -> Result<ExitCode> {
        if let Some(ref mut history) = self.cmd_history {
            history.add(&input);
            history.reset_index();
        }
        // TODO refactor context
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
                debug!("job exit code {:?}", exit);
                if exit != 0 {
                    return Ok(ExitCode::from(exit));
                }
            } else {
                // Stop next job
            }
            enable_raw_mode().ok();
        }

        Ok(ExitCode::from(0))
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

    fn parse_command(&mut self, job: &mut Job, pair: Pair<Rule>, foreground: bool) -> Result<()> {
        let parsed = self.get_argv(pair)?;
        if parsed.is_empty() {
            return Ok(());
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
        if let Some(cmd_fn) = builtin::get_command(cmd) {
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
            bail!("unknown command: {}", cmd);
        }
        Ok(())
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
        self.exited = Some(ExitStatus::ExitedWith(0));
    }
}
