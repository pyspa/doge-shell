use crate::direnv;
use crate::dirs;
use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::lisp;
use crate::parser::{self, Rule, ShellParser};
use crate::process::SubshellType;
use crate::process::{self, Job, JobProcess, Redirect, wait_pid_job};
use anyhow::Context as _;
use anyhow::{Result, anyhow, bail};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_types::{Context, ExitStatus};
use dsh_wasm::WasmEngine;
use libc::{STDIN_FILENO, c_int};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::sys::termios::tcgetattr;
use nix::unistd::{ForkResult, Pid, close, fork, getpid, pipe, setpgid};
use parking_lot::RwLock;
use pest::Parser;
use pest::iterators::Pair;
use std::fs::File;
use std::io::Write;
use std::io::prelude::*;
use std::os::fd::RawFd;
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::{cell::RefCell, rc::Rc};
use tracing::{debug, warn};

pub const APP_NAME: &str = "dsh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;

#[derive(Debug)]
struct ParsedJob {
    subshell_type: SubshellType,
    jobs: Vec<process::Job>,
}

impl ParsedJob {
    fn new(subshell_type: SubshellType, jobs: Vec<process::Job>) -> Self {
        ParsedJob {
            subshell_type,
            jobs,
        }
    }
}

#[derive(Debug)]
struct ParseContext {
    pub subshell: bool,
    pub proc_subst: bool,
    pub foreground: bool,
}

impl ParseContext {
    pub fn new(foreground: bool) -> Self {
        ParseContext {
            subshell: false,
            proc_subst: false,
            foreground,
        }
    }
}

pub struct Shell {
    pub environment: Arc<RwLock<Environment>>,
    pub exited: Option<ExitStatus>,
    pub pid: Pid,
    pub pgid: Pid,
    pub cmd_history: Option<Arc<Mutex<FrecencyHistory>>>,
    pub path_history: Option<Arc<Mutex<FrecencyHistory>>>,
    pub(crate) wait_jobs: Vec<Job>,
    pub lisp_engine: Rc<RefCell<lisp::LispEngine>>,
    pub wasm_engine: WasmEngine,
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
    pub fn new(environment: Arc<RwLock<Environment>>) -> Self {
        let pid = getpid();
        let pgid = pid;

        let _ = setpgid(pgid, pgid).context("failed setpgid");
        let cmd_history = FrecencyHistory::from_file("dsh_cmd_history").unwrap();
        let cmd_history = Arc::new(Mutex::new(cmd_history));

        let path_history = FrecencyHistory::from_file("dsh_path_history").unwrap();
        let path_history = Arc::new(Mutex::new(path_history));

        let wasm_engine = WasmEngine::new(APP_NAME);
        let lisp_engine = lisp::LispEngine::new(Arc::clone(&environment));
        if let Err(err) = lisp_engine.borrow().run_config_lisp() {
            eprintln!("failed load init lisp {err:?}");
        }

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
        eprint!("\r{msg}\r\n");
        std::io::stderr().flush().ok();
    }

    // pub fn print_stdout(&self, msg: &str) {
    //     // unknown command, etc
    //     print!("\r{msg}\r\n");
    //     std::io::stdout().flush().ok();
    // }

    pub async fn eval_str(
        &mut self,
        ctx: &mut Context,
        input: String,
        force_background: bool,
    ) -> Result<ExitCode> {
        if ctx.save_history {
            if let Some(ref mut history) = self.cmd_history {
                let mut history = history.lock().unwrap();
                history.add(&input);
                history.reset_index();
            }
        }
        // TODO refactor context
        // let tmode = tcgetattr(0).expect("failed tcgetattr");

        let jobs = self.get_jobs(input)?;
        let mut last_exit_code = 0;
        for mut job in jobs {
            disable_raw_mode().ok();
            if force_background {
                // all job run background
                job.foreground = false;
            }

            job.job_id = self.get_job_id(); // set job id

            debug!(
                "start job '{:?}' foreground:{:?} redirect:{:?} list_op:{:?}",
                job.cmd, job.foreground, job.redirect, job.list_op,
            );

            match job.launch(ctx, self).await? {
                process::ProcessState::Running => {
                    debug!("job '{}' still running", job.cmd);
                    self.wait_jobs.push(job);
                }
                process::ProcessState::Stopped(pid, _signal) => {
                    debug!("job '{}' stopped pid: {:?}", job.cmd, pid);
                    self.wait_jobs.push(job);
                }
                process::ProcessState::Completed(exit, _signal) => {
                    debug!("job '{}' completed exit_code: {:?}", job.cmd, exit);
                    last_exit_code = exit;
                    if job.list_op == process::ListOp::And && exit != 0 {
                        break;
                    }
                }
            }
            // reset
            ctx.pid = None;
            ctx.pgid = None;
            enable_raw_mode().ok();
        }

        enable_raw_mode().ok();
        Ok(ExitCode::from(last_exit_code))
    }

    fn get_jobs(&mut self, input: String) -> Result<Vec<Job>> {
        // TODO tests

        let input = parser::expand_alias(input, Arc::clone(&self.environment))?;

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
    ) -> Result<Vec<(String, Option<ParsedJob>)>> {
        let mut argv: Vec<(String, Option<ParsedJob>)> = vec![];

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::argv0 => {
                    for inner_pair in inner_pair.into_inner() {
                        // span
                        for inner_pair in inner_pair.into_inner() {
                            match inner_pair.as_rule() {
                                Rule::subshell => {
                                    debug!("find subshell arg0");
                                    for inner_pair in inner_pair.into_inner() {
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        // subshell
                                        let mut ctx = ParseContext::new(ctx.foreground);
                                        ctx.subshell = true;
                                        let res = self.parse_commands(&mut ctx, inner_pair)?;
                                        argv.push((
                                            cmd_str,
                                            Some(ParsedJob::new(SubshellType::Subshell, res)),
                                        ));
                                    }
                                }
                                Rule::proc_subst => {
                                    for inner_pair in inner_pair.into_inner() {
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        let mut ctx = ParseContext::new(ctx.foreground);
                                        ctx.proc_subst = true;
                                        let res = self.parse_commands(&mut ctx, inner_pair)?;
                                        argv.push((
                                            cmd_str,
                                            Some(ParsedJob::new(
                                                SubshellType::ProcessSubstitution,
                                                res,
                                            )),
                                        ));
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
                            let mut redirect_rule = None;
                            for pair in inner_pair.into_inner() {
                                if let Rule::stdout_redirect_direction
                                | Rule::stderr_redirect_direction
                                | Rule::stdouterr_redirect_direction = pair.as_rule()
                                {
                                    if let Some(rule) = pair.into_inner().next() {
                                        redirect_rule = Some(rule.as_rule());
                                    }
                                } else if let Rule::span = pair.as_rule() {
                                    let dest = pair.as_str();

                                    let redirect = match redirect_rule {
                                        Some(Rule::stdout_redirect_direction_out) => {
                                            Some(Redirect::StdoutOutput(dest.to_string()))
                                        }
                                        Some(Rule::stdout_redirect_direction_append) => {
                                            Some(Redirect::StdoutAppend(dest.to_string()))
                                        }

                                        Some(Rule::stderr_redirect_direction_out) => {
                                            Some(Redirect::StderrOutput(dest.to_string()))
                                        }
                                        Some(Rule::stderr_redirect_direction_append) => {
                                            Some(Redirect::StderrAppend(dest.to_string()))
                                        }

                                        Some(Rule::stdouterr_redirect_direction_out) => {
                                            Some(Redirect::StdouterrOutput(dest.to_string()))
                                        }
                                        Some(Rule::stdouterr_redirect_direction_append) => {
                                            Some(Redirect::StdouterrAppend(dest.to_string()))
                                        }
                                        _ => None,
                                    };
                                    current_job.redirect = redirect;
                                }
                            }
                            continue;
                        }

                        for inner_pair in inner_pair.into_inner() {
                            match inner_pair.as_rule() {
                                Rule::subshell => {
                                    debug!("find subshell args");
                                    for inner_pair in inner_pair.into_inner() {
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        // subshell
                                        let mut ctx = ParseContext::new(ctx.foreground);
                                        ctx.subshell = true;
                                        let res = self.parse_commands(&mut ctx, inner_pair)?;
                                        argv.push((
                                            cmd_str,
                                            Some(ParsedJob::new(SubshellType::Subshell, res)),
                                        ));
                                    }
                                }
                                Rule::proc_subst => {
                                    debug!("find proc_subs args");
                                    for inner_pair in inner_pair.into_inner() {
                                        if inner_pair.as_rule() == Rule::proc_subst_direction {
                                            continue;
                                        }
                                        // commands
                                        let cmd_str = inner_pair.as_str().to_string();
                                        let mut ctx = ParseContext::new(ctx.foreground);
                                        ctx.proc_subst = true;
                                        let res = self.parse_commands(&mut ctx, inner_pair)?;
                                        argv.push((
                                            cmd_str,
                                            Some(ParsedJob::new(
                                                SubshellType::ProcessSubstitution,
                                                res,
                                            )),
                                        ));
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
                        if let Some(sep) = pair.into_inner().next() {
                            if let Some(ref mut last) = jobs.last_mut() {
                                debug!("last job {:?}", &last.cmd);
                                match sep.as_rule() {
                                    Rule::and_op => {
                                        last.list_op = process::ListOp::And;
                                    }
                                    Rule::or_op => {
                                        last.list_op = process::ListOp::Or;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {
                        debug!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }

        debug!("parsed jobs len: {}", jobs.len());
        Ok(jobs)
    }

    fn launch_subshell(&mut self, ctx: &mut Context, jobs: Vec<Job>) -> Result<()> {
        for mut job in jobs {
            disable_raw_mode().ok();
            let pid =
                tokio::runtime::Handle::current().block_on(self.spawn_subshell(ctx, &mut job))?;
            debug!("spawned subshell cmd:{} pid: {:?}", job.cmd, pid);
            let res = wait_pid_job(pid, false);
            debug!("wait subshell exit:{:?}", res);
            enable_raw_mode().ok();
        }

        Ok(())
    }

    async fn spawn_subshell(&mut self, ctx: &mut Context, job: &mut Job) -> Result<Pid> {
        let pid = unsafe { fork().context("failed fork")? };

        match pid {
            ForkResult::Parent { child } => {
                let pid = child;
                debug!("subshell parent setpgid parent pid:{} pgid:{}", pid, pid);
                setpgid(pid, pid).context("failed setpgid")?;
                Ok(pid)
            }
            ForkResult::Child => {
                let pid = getpid();
                debug!("subshell child setpgid pid:{} pgid:{}", pid, pid);
                setpgid(pid, pid).context("failed setpgid")?;

                job.pgid = Some(pid);
                ctx.pgid = Some(pid);
                debug!("subshell run context: {:?}", ctx);
                let res = job.launch(ctx, self).await;
                debug!("subshell process '{}' exit:{:?}", job.cmd, res);

                if let Ok(process::ProcessState::Completed(exit, _)) = res {
                    if exit != 0 {
                        // TODO check
                        debug!("job exit code {:?}", exit);
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
        debug!("start parse command: {}", pair.as_str());
        let parsed_argv = self.parse_argv(ctx, current_job, pair)?;
        if parsed_argv.is_empty() {
            return Ok(());
        }

        let mut argv: Vec<String> = Vec::new();

        for (cmd_str, jobs) in parsed_argv {
            if let Some(ParsedJob {
                subshell_type,
                jobs,
            }) = jobs
            {
                debug!("parsed job '{:?}' jobs:{:?}", cmd_str, jobs);
                if jobs.is_empty() {
                    continue;
                }
                debug!("run subshell: {}", cmd_str);

                let tmode = tcgetattr(0).expect("failed tcgetattr");

                match subshell_type {
                    SubshellType::Subshell => {
                        let mut ctx = Context::new(self.pid, self.pgid, tmode, false);
                        ctx.foreground = true;
                        // make pipe
                        let (pout, pin) = pipe().context("failed pipe")?;
                        ctx.outfile = pin;
                        self.launch_subshell(&mut ctx, jobs)?;
                        close(pin).expect("failed close");
                        let output = read_fd(pout)?;
                        output.lines().for_each(|x| argv.push(x.to_owned()));
                    }
                    SubshellType::ProcessSubstitution => {
                        let mut ctx = Context::new(self.pid, self.pgid, tmode, false);
                        ctx.foreground = true;
                        // make pipe
                        let (pout, pin) = pipe().context("failed pipe")?;
                        ctx.outfile = pin;
                        self.launch_subshell(&mut ctx, jobs)?;
                        close(pin).expect("failed close");
                        let file_name = format!("/dev/fd/{}", pout);
                        argv.push(file_name);
                    }
                    _ => {}
                }
            } else {
                argv.push(cmd_str);
            }
        }

        if argv.is_empty() {
            // no main command
            return Ok(());
        }

        let cmd = argv[0].as_str();
        if let Some(cmd_fn) = dsh_builtin::get_command(cmd) {
            let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
            current_job.set_process(JobProcess::Builtin(builtin));
        } else if self.wasm_engine.modules.contains_key(cmd) {
            let wasm = process::WasmProcess::new(cmd.to_string(), argv);
            current_job.set_process(JobProcess::Wasm(wasm));
        } else if self.lisp_engine.borrow().is_export(cmd) {
            let cmd_fn = dsh_builtin::lisp::run;
            let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
            current_job.set_process(JobProcess::Builtin(builtin));
        } else if let Some(cmd) = self.environment.read().lookup(cmd) {
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
            debug!(
                "find {:?}:'{:?}'",
                inner_pair.as_rule(),
                inner_pair.as_str()
            );
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let mut job = Job::new(job_str.clone(), self.pgid);
                    self.parse_command(ctx, &mut job, inner_pair)?;
                    if job.has_process() {
                        if ctx.subshell {
                            job.subshell = SubshellType::Subshell;
                        }
                        if ctx.proc_subst {
                            job.subshell = SubshellType::ProcessSubstitution;
                        }
                        jobs.push(job);
                    }
                }
                Rule::simple_command_bg => {
                    // background job
                    let mut job = Job::new(inner_pair.as_str().to_string(), self.pgid);
                    for bg_pair in inner_pair.into_inner() {
                        if let Rule::simple_command = bg_pair.as_rule() {
                            self.parse_command(ctx, &mut job, bg_pair)?;
                            if job.has_process() {
                                if ctx.subshell {
                                    job.subshell = SubshellType::Subshell;
                                }
                                if ctx.proc_subst {
                                    job.subshell = SubshellType::ProcessSubstitution;
                                }
                                job.foreground = false; // background
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
                _ => {
                    warn!(
                        "missing rule {:?} {:?}",
                        inner_pair.as_rule(),
                        inner_pair.as_str()
                    );
                }
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

    pub fn exec_chpwd_hooks(&mut self, pwd: &str) -> Result<()> {
        let pwd = Path::new(pwd);

        chpwd_update_env(pwd, Arc::clone(&self.environment))?;
        direnv::check_path(pwd, Arc::clone(&self.environment))?;

        for hook in &self.environment.read().chpwd_hooks {
            hook.call(pwd, Arc::clone(&self.environment))?;
        }
        Ok(())
    }

    pub fn get_job_id(&self) -> usize {
        if self.wait_jobs.is_empty() {
            1
        } else if let Some(job) = self.wait_jobs.last() {
            job.job_id + 1
        } else {
            1
        }
    }

    pub async fn check_job_state(&mut self) -> Result<Vec<Job>> {
        let mut completed: Vec<Job> = Vec::new();
        let mut i = 0;
        while i < self.wait_jobs.len() {
            if !self.wait_jobs[i].foreground {
                self.wait_jobs[i].check_background_all_output().await?;
            }
            if self.wait_jobs[i].update_status() {
                let removed_job = self.wait_jobs.remove(i);
                completed.push(removed_job);
            } else {
                i += 1;
            }
        }
        Ok(completed)
    }

    pub fn kill_wait_jobs(&mut self) -> Result<()> {
        let mut i = 0;
        while i < self.wait_jobs.len() {
            self.wait_jobs[i].kill()?;
            i += 1;
        }
        Ok(())
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

fn chpwd_update_env(pwd: &Path, _env: Arc<RwLock<Environment>>) -> Result<()> {
    debug!("chpwd update env {:?}", pwd);
    unsafe { std::env::set_var("PWD", pwd) };
    Ok(())
}

fn read_fd(fd: RawFd) -> Result<String> {
    let mut raw_stdout = Vec::new();
    unsafe { File::from_raw_fd(fd).read_to_end(&mut raw_stdout).ok() };

    let output = std::str::from_utf8(&raw_stdout)
        .inspect_err(|_err| {
            // TODO
            eprintln!("binary in variable/expansion is not supported");
        })?
        .trim_end_matches('\n')
        .to_owned();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_chpwd_update_env() {
        let test_path = PathBuf::from("/tmp/test");
        let env = Environment::new();

        let result = chpwd_update_env(&test_path, env);
        assert!(result.is_ok());

        // PWD環境変数が設定されていることを確認
        let pwd = std::env::var("PWD").unwrap_or_default();
        assert!(pwd.contains("test") || pwd == "/tmp/test");
    }
}
