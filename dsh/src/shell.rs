use crate::direnv;
use crate::dirs;
use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::lisp;
use crate::parser::{self, Rule, ShellParser};
use crate::process::Redirect;
use crate::process::{self, Job, JobProcess, WaitJob};
use anyhow::Context as _;
use anyhow::{anyhow, bail, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_types::{Context, ExitStatus};
use dsh_wasm::WasmEngine;
use libc::{c_int, STDIN_FILENO};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::tcgetattr;
use nix::unistd::{close, fork, getpid, pipe, setpgid, ForkResult, Pid};
use pest::iterators::Pair;
use pest::Parser;
use std::fs::File;
use std::io::prelude::*;
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::{cell::RefCell, rc::Rc};
use tracing::{debug, warn};

pub const APP_NAME: &str = "dsh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;
pub type CommandHook = fn(pwd: &Path, env: Rc<RefCell<Environment>>);

#[derive(Debug)]
struct ParseContext {
    pub subshell: bool,
    pub foreground: bool,
}

impl ParseContext {
    pub fn new(foreground: bool) -> Self {
        ParseContext {
            subshell: false,
            foreground,
        }
    }
}

pub struct Shell {
    pub environment: Rc<RefCell<Environment>>,
    pub exited: Option<ExitStatus>,
    pub pid: Pid,
    pub pgid: Pid,
    pub cmd_history: Option<FrecencyHistory>,
    pub path_history: Option<FrecencyHistory>,
    pub wait_jobs: Vec<WaitJob>,
    pub lisp_engine: Rc<RefCell<lisp::LispEngine>>,
    pub wasm_engine: WasmEngine,
    pub chpwd_hooks: Vec<CommandHook>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("Shell")
            .field("environment", &self.environment)
            .field("pid", &self.pid)
            .field("pgid", &self.pgid)
            .finish()
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        disable_raw_mode().ok();
    }
}

impl Shell {
    pub fn new(environment: Rc<RefCell<Environment>>) -> Self {
        let pid = getpid();
        let pgid = pid;

        let _ = setpgid(pgid, pgid).context("failed setpgid");
        let cmd_history = FrecencyHistory::from_file("dsh_cmd_history").unwrap();
        let path_history = FrecencyHistory::from_file("dsh_path_history").unwrap();
        let wasm_engine = WasmEngine::new(APP_NAME);
        let lisp_engine = lisp::LispEngine::new(Rc::clone(&environment));
        if let Err(err) = lisp_engine.borrow().run_config_lisp() {
            eprintln!("failed load init lisp {err:?}");
        }
        debug!("dump environment {:?}", environment);
        let chpwd_hooks: Vec<CommandHook> = Vec::new();

        Shell {
            environment,
            exited: None::<ExitStatus>,
            pid,
            pgid,
            cmd_history: Some(cmd_history),
            path_history: Some(path_history),
            wait_jobs: Vec::new(),
            lisp_engine,
            wasm_engine,
            chpwd_hooks,
        }
    }

    pub fn install_chpwd_hooks(&mut self) {
        self.chpwd_hooks.push(chpwd_debug);
        self.chpwd_hooks.push(check_direnv);
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
        eprint!("\r{msg}\r\n");
        std::io::stderr().flush().ok();
    }

    pub fn print_stdout(&self, msg: String) {
        // unknown command, etc
        print!("\r{msg}\r\n");
        std::io::stdout().flush().ok();
    }

    pub fn eval_str(
        &mut self,
        mut ctx: Context,
        input: String,
        background: bool,
    ) -> Result<ExitCode> {
        if ctx.save_history {
            if let Some(ref mut history) = self.cmd_history {
                history.add(&input);
                history.reset_index();
            }
        }
        // TODO refactor context
        // let tmode = tcgetattr(0).expect("failed tcgetattr");

        let jobs = self.get_jobs(input)?;

        for mut job in jobs {
            disable_raw_mode().ok();
            if background {
                // all job run background
                job.foreground = false;
            }

            debug!(
                "start job:'{:?}' foreground:{:?} redirect:{:?}",
                job.cmd, job.foreground, job.redirect
            );

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

        let input = parser::expand_alias(input, Rc::clone(&self.environment))?;

        let mut pairs = ShellParser::parse(Rule::commands, &input).map_err(|e| anyhow!(e))?;
        let mut ctx = ParseContext::new(true);
        if let Some(pair) = pairs.next() {
            self.parse_commands(&mut ctx, pair)
        } else {
            Ok(Vec::new())
        }
    }

    fn parse_argv(
        &mut self,
        ctx: &mut ParseContext,
        current_job: &mut Job,
        pair: Pair<Rule>,
    ) -> Result<Vec<(String, Option<Vec<process::Job>>)>> {
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
                                        // subshell
                                        let mut ctx = ParseContext::new(ctx.foreground);
                                        ctx.subshell = true;
                                        let res = self.parse_commands(&mut ctx, inner_pair)?;
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
                        if let Rule::redirect = inner_pair.as_rule() {
                            // set redirect
                            let mut direction = "";
                            for pair in inner_pair.into_inner() {
                                if let Rule::redirect_direction = pair.as_rule() {
                                    direction = pair.as_str();
                                }
                                let redirect = if let Rule::span = pair.as_rule() {
                                    if direction == ">>" {
                                        Some(Redirect::Append(pair.as_str().to_string()))
                                    } else {
                                        Some(Redirect::Output(pair.as_str().to_string()))
                                    }
                                } else {
                                    None
                                };
                                current_job.redirect = redirect;
                            }
                            continue;
                        }
                        for inner_pair in inner_pair.into_inner() {
                            match inner_pair.as_rule() {
                                Rule::subshell => {
                                    for inner_pair in inner_pair.into_inner() {
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        // subshell
                                        let mut ctx = ParseContext::new(ctx.foreground);
                                        ctx.subshell = true;
                                        let res = self.parse_commands(&mut ctx, inner_pair)?;
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
                    let mut res = self.parse_argv(ctx, current_job, inner_pair)?;
                    argv.append(&mut res);
                }
                _ => {
                    warn!("missing {:?}", inner_pair.as_rule());
                }
            }
        }
        Ok(argv)
    }

    fn parse_commands(&mut self, ctx: &mut ParseContext, pair: Pair<Rule>) -> Result<Vec<Job>> {
        let mut jobs: Vec<Job> = Vec::new();
        if let Rule::commands = pair.as_rule() {
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::command => self.parse_jobs(ctx, pair, &mut jobs)?,
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

    fn launch_subshell(&mut self, ctx: &mut Context, jobs: Vec<Job>) -> Result<()> {
        for mut job in jobs {
            disable_raw_mode().ok();
            let pid = self.spawn_subshell(ctx, &mut job)?;
            debug!("spawned subshell pid: {:?}", pid);
            let res = process::wait_pid(pid);
            debug!("wait subshell pid: {:?}", res);
            enable_raw_mode().ok();
        }

        Ok(())
    }

    fn spawn_subshell(&mut self, ctx: &mut Context, job: &mut Job) -> Result<Pid> {
        let pid = unsafe { fork().context("failed fork")? };
        match pid {
            ForkResult::Parent { child } => Ok(child),
            ForkResult::Child => {
                if let Ok(process::ProcessState::Completed(exit)) = job.launch(ctx, self) {
                    if exit != 0 {
                        // TODO check
                        debug!("\rjob exit code {:?}", exit);
                    }
                    std::process::exit(exit as i32);
                } else {
                    std::process::exit(-1);
                }
            }
        }
    }

    fn parse_command(
        &mut self,
        ctx: &mut ParseContext,
        current_job: &mut Job,
        pair: Pair<Rule>,
    ) -> Result<()> {
        let parsed_argv = self.parse_argv(ctx, current_job, pair)?;
        if parsed_argv.is_empty() {
            return Ok(());
        }

        let mut argv: Vec<String> = Vec::new();

        for (str, jobs) in parsed_argv {
            if let Some(jobs) = jobs {
                let tmode = tcgetattr(0).expect("failed tcgetattr");

                let mut ctx = Context::new(self.pid, self.pgid, tmode, false);
                // make pipe
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;

                self.launch_subshell(&mut ctx, jobs)?;
                close(pin).expect("failed close");

                let output = read_fd(pout)?;
                output.lines().for_each(|x| argv.push(x.to_owned()));
            } else {
                argv.push(str);
            }
        }

        let cmd = argv[0].as_str();
        if let Some(cmd_fn) = dsh_builtin::get_command(cmd) {
            let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
            current_job.set_process(JobProcess::Builtin(builtin));
        } else if self.wasm_engine.modules.get(cmd).is_some() {
            let wasm = process::WasmProcess::new(cmd.to_string(), argv);
            current_job.set_process(JobProcess::Wasm(wasm));
        } else if self.lisp_engine.borrow().is_export(cmd) {
            // let cmd_fn = builtin::lisp::run;
            // let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
            // job.set_process(JobProcess::Builtin(builtin));
        } else if let Some(cmd) = self.environment.borrow().lookup(cmd) {
            let process = process::Process::new(cmd, argv);
            current_job.set_process(JobProcess::Command(process));
            current_job.foreground = ctx.foreground;
        } else if dirs::is_dir(cmd) {
            if let Some(cmd_fn) = dsh_builtin::get_command("cd") {
                let builtin = process::BuiltinProcess::new(
                    cmd.to_string(),
                    cmd_fn,
                    vec!["cd".to_string(), cmd.to_string()],
                );
                current_job.set_process(JobProcess::Builtin(builtin));
            }
        } else {
            bail!("unknown command: {}", cmd);
        }
        Ok(())
    }

    fn parse_jobs(
        &mut self,
        ctx: &mut ParseContext,
        pair: Pair<Rule>,
        jobs: &mut Vec<Job>,
    ) -> Result<()> {
        let job_str = pair.as_str().to_string();

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let mut job = Job::new(job_str.clone());

                    self.parse_command(ctx, &mut job, inner_pair)?;
                    if job.process.is_some() {
                        job.subshell = ctx.subshell;
                        jobs.push(job);
                    }
                }
                Rule::simple_command_bg => {
                    // background job
                    let mut job = Job::new(inner_pair.as_str().to_string());
                    for bg_pair in inner_pair.into_inner() {
                        if let Rule::simple_command = bg_pair.as_rule() {
                            self.parse_command(ctx, &mut job, bg_pair)?;
                            if job.process.is_some() {
                                job.subshell = ctx.subshell;
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
                                ctx.foreground = true;
                                self.parse_command(ctx, job, inner_pair)?;
                            } else if let Rule::simple_command_bg = inner_pair.as_rule() {
                                ctx.foreground = false;
                                self.parse_command(ctx, job, inner_pair)?;
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
        self.wasm_engine.call(name, args.as_ref())
    }

    pub fn exit(&mut self) {
        self.exited = Some(ExitStatus::ExitedWith(0));
    }

    pub fn chpwd(&mut self, pwd: &str) {
        let pwd = Path::new(pwd);
        for hook in &self.chpwd_hooks {
            hook(pwd, Rc::clone(&self.environment));
        }
    }

    // pub fn chpwd2(&mut self, pwd: &str) {
    //     let env = Rc::clone(&self.environment);
    //     let hooks = env.borrow().chpwd_hooks.;
    //     let pwd = Path::new(pwd);
    //     for hook in hooks {
    //         hook(pwd, Rc::clone(&env));
    //     }
    // }
}

fn chpwd_debug(pwd: &Path, _env: Rc<RefCell<Environment>>) {
    debug!("chpwd {:?}", pwd);
}

fn check_direnv(pwd: &Path, env: Rc<RefCell<Environment>>) {
    direnv::check_path(pwd, &mut env.borrow_mut().direnv_roots);
}

fn read_fd(fd: RawFd) -> Result<String> {
    let mut raw_stdout = Vec::new();
    unsafe { File::from_raw_fd(fd).read_to_end(&mut raw_stdout).ok() };

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
