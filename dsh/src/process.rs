use crate::shell::{Shell, SHELL_TERMINAL};
use anyhow::Context as _;
use anyhow::Result;
use async_std::io::prelude::BufReadExt;
use async_std::{fs, io, task};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::BuiltinCommand;
use dsh_types::{Context, ExitStatus};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{kill, killpg, sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::Termios;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{
    close, dup2, execv, fork, getpgrp, getpid, pipe, setpgid, tcsetpgrp, ForkResult, Pid,
};
use std::ffi::CString;
use std::fmt::Debug;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;
use std::time::Duration;
use tracing::{debug, error};

const MONITOR_TIMEOUT: u64 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirect {
    StdoutOutput(String),
    StdoutAppend(String),
    StderrOutput(String),
    StderrAppend(String),
    StdouterrOutput(String),
    StdouterrAppend(String),
    Input,
}

impl Redirect {
    fn process(&self, ctx: &mut Context) {
        match self {
            Redirect::StdoutOutput(out)
            | Redirect::StderrOutput(out)
            | Redirect::StdouterrOutput(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                task::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    let mut writer = fs::File::create(file.to_string()).await.unwrap(); // TODO check err
                    let _res = io::copy(&mut reader, &mut writer).await; // TODO check err
                });
            }

            Redirect::StdoutAppend(out)
            | Redirect::StderrAppend(out)
            | Redirect::StdouterrAppend(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                task::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    let mut writer = fs::OpenOptions::new()
                        .write(true)
                        .append(true)
                        .open(file.to_string())
                        .await
                        .unwrap();
                    let _res = io::copy(&mut reader, &mut writer).await; // TODO check err
                });
            }
            Redirect::Input => {}
        }
    }
}

fn copy_fd(src: RawFd, dst: RawFd) {
    if src != dst {
        dup2(src, dst).expect("failed dup2");
        close(src).expect("failed close");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOp {
    None,
    And,
    Or,
}

#[derive(Clone)]
pub struct BuiltinProcess {
    name: String,
    cmd_fn: BuiltinCommand,
    argv: Vec<String>,
    state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl PartialEq for BuiltinProcess {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv
    }
}

impl Eq for BuiltinProcess {}

impl std::fmt::Debug for BuiltinProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("BuiltinProcess")
            .field("argv", &self.argv)
            .field("state", &self.state)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl BuiltinProcess {
    pub fn new(name: String, cmd_fn: BuiltinCommand, argv: Vec<String>) -> Self {
        BuiltinProcess {
            name,
            cmd_fn,
            argv,
            state: ProcessState::Running,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(ref mut next) = self.next {
            if next.set_state_pid(pid, state) {
                return true;
            }
        }
        return false;
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                self.next = Some(Box::new(process));
            }
        }
    }

    pub fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        debug!(
            "launch: builtin process {:?} infile:{:?} outfile:{:?}",
            &self.name, self.stdin, self.stdout
        );
        let exit = (self.cmd_fn)(ctx, self.argv.to_vec(), shell);
        if let ExitStatus::ExitedWith(code) = exit {
            if code >= 0 {
                self.state = ProcessState::Completed(0);
            }
        }
        // TODO check exit
        Ok(())
    }
}

#[derive(Clone)]
pub struct WasmProcess {
    name: String,
    argv: Vec<String>,
    state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl PartialEq for WasmProcess {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv
    }
}

impl Eq for WasmProcess {}

impl std::fmt::Debug for WasmProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("WASMProcess")
            .field("name", &self.name)
            .field("argv", &self.argv)
            .field("state", &self.state)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl WasmProcess {
    pub fn new(name: String, argv: Vec<String>) -> Self {
        WasmProcess {
            name,
            argv,
            state: ProcessState::Running,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(ref mut next) = self.next {
            if next.set_state_pid(pid, state) {
                return true;
            }
        }
        return false;
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                self.next = Some(Box::new(process));
            }
        }
    }

    pub fn launch(&mut self, _ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        debug!(
            "launch: wasm process infile:{:?} outfile:{:?}",
            self.stdin, self.stdout
        );

        // TODO check exit
        shell.run_wasm(self.name.as_str(), self.argv.to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    cmd: String,
    argv: Vec<String>,
    pid: Option<Pid>,
    status: Option<ExitStatus>,
    state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    cap_stdout: Option<RawFd>,
    cap_stderr: Option<RawFd>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Completed(u8),
    Stopped(Pid),
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProcessState::Running => formatter.write_str("running"),
            ProcessState::Completed(_) => formatter.write_str("completed"),
            ProcessState::Stopped(_) => formatter.write_str("stopped"),
        }
    }
}

impl Process {
    pub fn new(cmd: String, argv: Vec<String>) -> Self {
        Process {
            cmd,
            argv,
            pid: None,
            status: None,
            state: ProcessState::Running,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            cap_stdout: None,
            cap_stderr: None,
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(ppid) = self.pid {
            if ppid == pid {
                self.state = state;
                return true;
            }
        }
        if let Some(ref mut next) = self.next {
            if next.set_state_pid(pid, state) {
                return true;
            }
        }
        return false;
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                debug!("link:{} next:{}", self.cmd, process.get_cmd());
                self.next = Some(Box::new(process));
            }
        }
    }

    fn set_signals(&self) {
        // Accept job-control-related signals (refer https://www.gnu.org/software/libc/manual/html_node/Launching-Jobs.html)
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action).expect("failed to sigaction");
            sigaction(Signal::SIGQUIT, &action).expect("failed to sigaction");
            sigaction(Signal::SIGTSTP, &action).expect("failed to sigaction");
            sigaction(Signal::SIGTTIN, &action).expect("failed to sigaction");
            sigaction(Signal::SIGTTOU, &action).expect("failed to sigaction");
            sigaction(Signal::SIGCHLD, &action).expect("failed to sigaction");
        }
    }

    pub fn launch(
        &mut self,
        pid: Pid,
        pgid: Pid,
        interactive: bool,
        foreground: bool,
    ) -> Result<()> {
        if interactive {
            debug!(
                "setpgid child process {} pid:{} pgid:{} foreground:{}",
                &self.cmd, pid, pgid, foreground
            );
            setpgid(pid, pgid).context("failed setpgid")?;

            if foreground {
                tcsetpgrp(SHELL_TERMINAL, pgid).context("failed tcsetpgrp")?;
            }

            self.set_signals();
        }

        let cmd = CString::new(self.cmd.clone()).context("failed new CString")?;
        let argv: Vec<CString> = self
            .argv
            .clone()
            .into_iter()
            .map(|a| CString::new(a).expect("failed new CString"))
            .collect();

        debug!(
            "launch: execv cmd:{:?} argv:{:?} foreground:{:?} infile:{:?} outfile:{:?} pid:{:?} pgid:{:?}",
            cmd, argv, foreground, self.stdin, self.stdout,pid, pgid,
        );

        copy_fd(self.stdin, STDIN_FILENO);
        if self.stdout == self.stderr {
            dup2(self.stdout, STDOUT_FILENO).expect("failed dup2");
            dup2(self.stderr, STDERR_FILENO).expect("failed dup2");
            close(self.stdout).expect("failed close");
        } else {
            copy_fd(self.stdout, STDOUT_FILENO);
            copy_fd(self.stderr, STDERR_FILENO);
        }
        match execv(&cmd, &argv) {
            Ok(_) => Ok(()),
            Err(nix::errno::Errno::EACCES) => {
                error!("Failed to exec {:?} (EACCESS). chmod(1) may help.", cmd);
                std::process::exit(1);
            }
            Err(err) => {
                error!("Failed to exec {:?} ({})", cmd, err);
                std::process::exit(1);
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Wasm(WasmProcess),
    Command(Process),
}

impl std::fmt::Debug for JobProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        match self {
            JobProcess::Builtin(jprocess) => f
                .debug_struct("JobProcess::Builtin")
                .field("cmd", &jprocess.argv)
                .field("has_next", &jprocess.next.is_some())
                .finish(),
            JobProcess::Wasm(jprocess) => f
                .debug_struct("JobProcess::WASM")
                .field("wasm", jprocess)
                .field("has_next", &jprocess.next.is_some())
                .finish(),
            JobProcess::Command(jprocess) => f
                .debug_struct("JobProcess::Command")
                .field("cmd", &jprocess.cmd)
                .field("argv", &jprocess.argv)
                .field("pid", &jprocess.pid)
                .field("stdin", &jprocess.stdin)
                .field("stdout", &jprocess.stdout)
                .field("stderr", &jprocess.stderr)
                .field("has_next", &jprocess.next.is_some())
                .field("state", &jprocess.state)
                .finish(),
        }
    }
}

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Wasm(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Wasm(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn mut_next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Wasm(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn take_next(&mut self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.take(),
            JobProcess::Wasm(jprocess) => jprocess.next.take(),
            JobProcess::Command(jprocess) => jprocess.next.take(),
        }
    }

    pub fn set_io(&mut self, stdin: RawFd, stdout: RawFd, stderr: RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::Wasm(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::Command(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
        }
    }

    pub fn get_io(&self) -> (RawFd, RawFd, RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Wasm(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(_) => {
                // noop
            }
            JobProcess::Wasm(_) => {
                // noop
            }
            JobProcess::Command(process) => {
                process.pid = pid;
            }
        }
    }

    pub fn get_pid(&self) -> Option<Pid> {
        match self {
            JobProcess::Builtin(_) => {
                // noop
                None
            }
            JobProcess::Wasm(_) => {
                // noop
                None
            }
            JobProcess::Command(process) => process.pid,
        }
    }

    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Wasm(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
        }
    }

    fn set_state_pid(&mut self, pid: Pid, state: ProcessState) -> bool {
        match self {
            JobProcess::Builtin(p) => p.set_state(pid, state),
            JobProcess::Wasm(p) => p.set_state(pid, state),
            JobProcess::Command(p) => p.set_state(pid, state),
        }
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
            JobProcess::Wasm(p) => p.state,
            JobProcess::Command(p) => p.state,
        }
    }

    fn is_stopped(&self) -> bool {
        if self.get_state() == ProcessState::Running {
            return false;
        }
        if let Some(p) = self.next() {
            return p.is_stopped();
        }
        true
    }

    fn is_completed(&self) -> bool {
        match self.get_state() {
            ProcessState::Completed(_) => {
                //ok
                if let Some(next) = self.next() {
                    return next.is_completed();
                }
            }
            _ => {
                return false;
            }
        };
        true
    }

    pub fn get_cap_out(&self) -> (Option<RawFd>, Option<RawFd>) {
        match self {
            JobProcess::Builtin(_p) => (None, None),
            JobProcess::Wasm(_p) => (None, None),
            JobProcess::Command(p) => (p.cap_stdout, p.cap_stderr),
        }
    }

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
            JobProcess::Wasm(p) => &p.name,
            JobProcess::Command(p) => &p.cmd,
        }
    }

    pub fn waitable(&self) -> bool {
        match self {
            JobProcess::Command(_) => true,
            _ => false,
        }
    }

    pub fn launch(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        redirect: &Option<Redirect>,
        stdout: RawFd,
    ) -> Result<(Pid, Option<Box<JobProcess>>)> {
        // has pipelines process ?
        let next_process = self.take_next();

        let pipe_out = match next_process {
            Some(_) => {
                create_pipe(ctx)? // create pipe
            }
            None => {
                handle_output_redirect(ctx, redirect, stdout)? // check redirect
            }
        };

        self.set_io(ctx.infile, ctx.outfile, ctx.errfile);

        debug!("start process ctx {:?}", &ctx);
        debug!("start process {:?}", &self);

        // initial pid
        let current_pid = getpid();

        let pid = match self {
            JobProcess::Builtin(process) => {
                if ctx.foreground {
                    process.launch(ctx, shell)?;
                } else {
                    // TODO fork
                    process.launch(ctx, shell)?;
                }
                current_pid
            }
            JobProcess::Wasm(process) => {
                if ctx.foreground {
                    process.launch(ctx, shell)?;
                } else {
                    // TODO fork
                    process.launch(ctx, shell)?;
                }
                current_pid
            }
            JobProcess::Command(process) => {
                ctx.process_count += 1;
                // fork
                let pid = fork_process(ctx, ctx.pgid, process)?;

                pid
            }
        };

        self.set_pid(Some(pid));

        // set pipe inout
        if let Some(pipe_out) = pipe_out {
            ctx.infile = pipe_out;
        }
        // return launched process pid and pipeline process
        Ok((pid, next_process))
    }

    pub fn kill(&self) -> Result<()> {
        match self {
            JobProcess::Builtin(_) => Ok(()),
            JobProcess::Wasm(_) => Ok(()),
            JobProcess::Command(process) => {
                if let Some(pid) = process.pid {
                    send_signal(pid, Signal::SIGKILL)
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub cmd: String,
    pub pid: Option<Pid>,
    pub pgid: Option<Pid>,
    pub process: Option<Box<JobProcess>>,
    notified: bool,
    tmodes: Option<Termios>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub next: Option<Box<Job>>,
    pub foreground: bool,
    pub subshell: bool,
    pub redirect: Option<Redirect>,
    pub list_op: ListOp,
    pub job_id: usize,
    pub state: ProcessState,
    monitors: Vec<OutputMonitor>,
    shell_pgid: Pid,
}

impl Job {
    pub fn new_with_process(cmd: String, path: String, argv: Vec<String>) -> Self {
        let process = JobProcess::Command(Process::new(path, argv));
        let id = format!("{}", xid::new());
        let shell_pgid = getpgrp();
        Job {
            id,
            cmd,
            pid: None,
            pgid: None,
            process: Some(Box::new(process)),
            notified: false,
            tmodes: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            next: None,
            foreground: true,
            subshell: false,
            redirect: None,
            list_op: ListOp::None,
            job_id: 1,
            state: ProcessState::Running,
            monitors: Vec::new(),
            shell_pgid,
        }
    }

    pub fn new(cmd: String, shell_pgid: Pid) -> Self {
        let id = format!("{}", xid::new());
        Job {
            id,
            cmd,
            pid: None,
            pgid: None,
            process: None,
            notified: false,
            tmodes: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            next: None,
            foreground: true,
            subshell: false,
            redirect: None,
            list_op: ListOp::None,
            job_id: 1,
            state: ProcessState::Running,
            monitors: Vec::new(),
            shell_pgid,
        }
    }

    pub fn link(&mut self, job: Job) {
        match self.next {
            Some(ref mut j) => {
                j.link(job);
            }
            None => {
                self.next = Some(Box::new(job));
            }
        }
    }

    pub fn set_process(&mut self, process: JobProcess) {
        match self.process {
            Some(ref mut p) => p.link(process),
            None => self.process = Some(Box::new(process)),
        }
    }

    pub fn last_process_state(&self) -> ProcessState {
        if let Some(ref p) = &self.process {
            last_process_state(*p.clone())
        } else {
            // not running
            ProcessState::Completed(0)
        }
    }

    pub async fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<ProcessState> {
        ctx.foreground = self.foreground;

        if let Some(process) = self.process.take().as_mut() {
            let _ = self.launch_process(ctx, shell, process);
            if !ctx.interactive {
                self.wait_job();
            } else if ctx.foreground {
                // foreground
                if ctx.process_count > 0 {
                    let _ = self.put_in_foreground();
                }
            } else {
                // background
                let _ = self.put_in_background();
            }
        }

        debug!("lauched job context {:?}", ctx);
        if ctx.foreground {
            Ok(self.last_process_state())
        } else {
            // background
            Ok(ProcessState::Running)
        }
    }

    pub fn launch_process(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        process: &mut JobProcess,
    ) -> Result<()> {
        let (pid, mut next_process) = process.launch(ctx, shell, &self.redirect, self.stdout)?;

        self.pid = Some(pid); // set process pid
        self.state = process.get_state();

        if ctx.interactive {
            if self.pgid.is_none() {
                self.pgid = Some(pid);
                ctx.pgid = Some(pid);
                debug!("set job pgid {:?}", self.pgid);
            }
            debug!(
                "setpgid {} pid:{} pgid:{:?}",
                process.get_cmd(),
                pid,
                self.pgid
            );
            setpgid(pid, self.pgid.unwrap_or(pid))?;
        }

        let (stdout, stderr) = process.get_cap_out();
        if let Some(stdout) = stdout {
            let monitor = OutputMonitor::new(stdout);
            self.monitors.push(monitor);
        }

        if let Some(stderr) = stderr {
            let monitor = OutputMonitor::new(stderr);
            self.monitors.push(monitor);
        }

        let (stdin, stdout, stderr) = process.get_io();
        if stdin != self.stdin {
            close(stdin).context("failed close")?;
        }
        if stdout != self.stdout {
            close(stdout).context("failed close stdout")?;
        }
        if stderr != self.stderr && stdout != stderr {
            close(stderr).context("failed close stderr")?;
        }

        self.set_process(process.to_owned());
        self.show_job_status();

        if let Some(ref redirect) = self.redirect {
            redirect.process(ctx);
        }
        // run next pipeline process
        if let Some(Err(err)) = next_process
            .take()
            .as_mut()
            .map(|process| self.launch_process(ctx, shell, process))
        {
            debug!("err {:?}", err);
            return Err(err);
        }

        Ok(())
    }

    fn put_in_foreground(&mut self) -> Result<()> {
        debug!("put_in_foreground: pgid {:?}", self.pgid);
        // Put the job into the foreground

        tcsetpgrp(SHELL_TERMINAL, self.pgid.unwrap()).context("failed tcsetpgrp")?;
        // TODO Send the job a continue signal, if necessary.

        self.wait_job();

        tcsetpgrp(SHELL_TERMINAL, self.shell_pgid).context("failed tcsetpgrp shell_pgid")?;
        // debug!("put_in_foreground: {:?} tcsetpgrp shell", &self.cmd);

        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        // let _ = tcsetattr(SHELL_TERMINAL, TCSADRAIN, &ctx.shell_tmode)
        //     .context("failed tcsetattr restore shell_mode")?;

        Ok(())
    }

    fn put_in_background(&mut self) -> Result<()> {
        debug!("put_in_background pgid {:?}", self.pgid,);

        // TODO Send the job a continue signal, if necessary.

        // let _ = tcsetpgrp(SHELL_TERMINAL, ctx.shell_pgid).context("failed tcsetpgrp shell_pgid")?;
        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        Ok(())
    }

    fn show_job_status(&self) {}

    pub fn wait_job(&mut self) {
        let mut send_killpg = false;
        loop {
            // TODO other process waitpid
            debug!("waitpid ...");
            let result = waitpid(None, Some(WaitPidFlag::WUNTRACED));

            let (pid, state) = match result {
                Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8)), // ok??
                Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(1)),
                Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
                Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
                    // break?
                    return;
                }
                status => {
                    panic!("unexpected waitpid event: {:?}", status);
                }
            };

            self.set_process_state(pid, state);

            debug!("fin wait: pid:{:?}", pid);

            show_process_state(&self.process); // debug

            if let ProcessState::Completed(code) = state {
                if code != 0 && !send_killpg {
                    if let Some(pgid) = self.pgid {
                        debug!("killpg pgid: {}", pgid);
                        let _ = killpg(pgid, Signal::SIGKILL);
                        send_killpg = true;
                    }
                }
            }
            // break;
            if is_job_completed(self) {
                break;
            }
            if is_job_stopped(self) {
                println!("\rdsh: job {} '{}' has stopped", self.job_id, self.cmd);
                break;
            }
        }
    }

    fn set_process_state(&mut self, pid: Pid, state: ProcessState) {
        if let Some(process) = self.process.as_mut() {
            process.set_state_pid(pid, state);
        }
    }

    pub async fn check_background_output(&mut self) -> Result<()> {
        let mut i = 0;
        while i < self.monitors.len() {
            let _ = self.monitors[i].output().await?;
            i += 1;
        }
        Ok(())
    }

    pub async fn check_background_all_output(&mut self) -> Result<()> {
        let mut i = 0;
        while i < self.monitors.len() {
            self.monitors[i].output_all().await?;
            i += 1;
        }
        Ok(())
    }

    pub fn kill(&mut self) -> Result<()> {
        kill_process(&self.process)
    }

    // pub async fn check_background_output2(&self) -> Result<()> {
    //     if let Some(ref process) = self.process {
    //         let (stdout, stderr) = process.get_cap_out();
    //         if let Some(stdout) = stdout {
    //             let file = unsafe { fs::File::from_raw_fd(stdout) };
    //             let mut reader = io::BufReader::new(file);
    //             let mut len = 1;
    //             while len != 0 {
    //                 let mut line = String::new();
    //                 len = reader.read_line(&mut line).await?;
    //                 disable_raw_mode().ok();
    //                 print!("{}", line);
    //                 enable_raw_mode().ok();
    //             }
    //         }
    //         if let Some(stderr) = stderr {
    //             let file = unsafe { fs::File::from_raw_fd(stderr) };
    //             let mut reader = io::BufReader::new(file);
    //             let mut len = 1;
    //             while len != 0 {
    //                 let mut line = String::new();
    //                 len = reader.read_line(&mut line).await?;
    //                 disable_raw_mode().ok();
    //                 print!("{}", line);
    //                 enable_raw_mode().ok();
    //             }
    //         }
    //     }
    //     Ok(())
    // }
}

pub struct OutputMonitor {
    reader: io::BufReader<fs::File>,
    outputed: bool,
}

impl std::fmt::Debug for OutputMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("OutputMonitor").finish()
    }
}

impl OutputMonitor {
    pub fn new(fd: RawFd) -> Self {
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let reader = io::BufReader::new(file);
        OutputMonitor {
            reader,
            outputed: false,
        }
    }

    pub async fn output(&mut self) -> Result<usize> {
        let mut line = String::new();
        match io::timeout(
            Duration::from_millis(MONITOR_TIMEOUT),
            self.reader.read_line(&mut line),
        )
        .await
        {
            Ok(len) => {
                disable_raw_mode().ok();
                if !self.outputed {
                    self.outputed = true;
                    print!("\n\r{}", line);
                } else {
                    print!("{}", line);
                }
                enable_raw_mode().ok();
                Ok(len)
            }
            Err(_err) => Ok(0),
        }
    }

    pub async fn output_all(&mut self) -> Result<()> {
        let mut len = 1;
        while len != 0 {
            let mut line = String::new();
            match io::timeout(
                Duration::from_millis(MONITOR_TIMEOUT),
                self.reader.read_line(&mut line),
            )
            .await
            {
                Ok(readed) => {
                    disable_raw_mode().ok();
                    if !self.outputed {
                        self.outputed = true;
                        print!("\r{}", line);
                    } else {
                        print!("{}", line);
                    }
                    enable_raw_mode().ok();
                    len = readed;
                }
                Err(_err) => {}
            }
        }
        Ok(())
    }
}

pub fn wait_any_job(no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    let result = waitpid(None, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8)),
        Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(1)),
        Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
        Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
            return None;
        }
        status => {
            panic!("unexpected waitpid event: {:?}", status);
        }
    };
    Some(res)
}

fn last_process_state(process: JobProcess) -> ProcessState {
    debug!(
        "last_process_state:{} {} has_next: {}",
        process.get_cmd(),
        process.get_state(),
        process.next().is_some(),
    );
    if let Some(next_proc) = process.next() {
        last_process_state(*next_proc)
    } else {
        process.get_state()
    }
}

fn show_process_state(process: &Option<Box<JobProcess>>) {
    if let Some(process) = process {
        debug!(
            "process_state: {:?} state:{:?}",
            process.get_cmd(),
            process.get_state(),
        );

        if let Some(_) = process.next() {
            show_process_state(&process.next());
        }
    }
}

fn fork_process(ctx: &Context, job_pgid: Option<Pid>, process: &mut Process) -> Result<Pid> {
    // capture
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground {
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stdout = pin;
        process.cap_stdout = Some(pout);
    }
    if ctx.errfile == STDERR_FILENO && !ctx.foreground {
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stderr = pin;
        process.cap_stderr = Some(pout);
    }

    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            // if process.stdout != STDOUT_FILENO {
            //     close(process.stdout).context("failed close")?;
            // }
            Ok(child)
        }
        ForkResult::Child => {
            // This is the child process
            let pid = getpid();
            let pgid = job_pgid.unwrap_or(pid);
            process.launch(pid, pgid, ctx.interactive, ctx.foreground)?;
            unreachable!();
        }
    }
}

pub fn is_job_stopped(job: &Job) -> bool {
    if let Some(p) = &job.process {
        p.is_stopped()
    } else {
        true
    }
}

pub fn is_job_completed(job: &Job) -> bool {
    if let Some(process) = &job.process {
        debug!(
            "is_job_completed {} {}",
            process.get_cmd(),
            process.get_state()
        );
        process.is_completed()
    } else {
        true
    }
}

pub fn wait_pid_job(pid: Pid, no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    let result = waitpid(pid, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8)),
        Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(1)),
        Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
        Err(nix::errno::Errno::ECHILD) => (pid, ProcessState::Completed(1)),
        Ok(WaitStatus::StillAlive) => {
            return None;
        }
        status => {
            panic!("unexpected waitpid event: {:?}", status);
        }
    };
    Some(res)
}

fn create_pipe(ctx: &mut Context) -> Result<Option<RawFd>> {
    let (pout, pin) = pipe().context("failed pipe")?;
    ctx.outfile = pin;
    Ok(Some(pout))
}

fn handle_output_redirect(
    ctx: &mut Context,
    redirect: &Option<Redirect>,
    stdout: RawFd,
) -> Result<Option<RawFd>> {
    if let Some(ref output) = redirect {
        match output {
            Redirect::StdoutOutput(_) | Redirect::StdoutAppend(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;
                Ok(Some(pout))
            }
            Redirect::StderrOutput(_) | Redirect::StderrAppend(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.errfile = pin;
                Ok(Some(pout))
            }
            Redirect::StdouterrOutput(_) | Redirect::StdouterrAppend(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;
                ctx.errfile = pin;
                Ok(Some(pout))
            }
            _ => Ok(None),
        }
    } else {
        if let Some(out) = ctx.captured_out {
            ctx.outfile = out;
        } else if ctx.infile != STDIN_FILENO {
            ctx.outfile = stdout;
        }
        Ok(None)
    }
}

fn send_signal(pid: Pid, signal: Signal) -> Result<()> {
    Ok(kill(pid, signal)?)
}

fn kill_process(process: &Option<Box<JobProcess>>) -> Result<()> {
    if let Some(process) = process {
        process.kill();
        if let Some(_) = process.next() {
            kill_process(&process.next())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use nix::sys::termios::tcgetattr;
    use nix::unistd::getpgrp;

    use super::*;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_find_job() {
        init();
        let pgid1 = Pid::from_raw(1);
        let pgid2 = Pid::from_raw(2);
        let pgid3 = Pid::from_raw(3);

        let mut job1 = Job::new_with_process("test1".to_owned(), "".to_owned(), vec![]);
        job1.pgid = Some(pgid1);
        let mut job2 = Job::new_with_process("test2".to_owned(), "".to_owned(), vec![]);
        job2.pgid = Some(pgid2);
        let mut job3 = Job::new_with_process("test3".to_owned(), "".to_owned(), vec![]);
        job3.pgid = Some(pgid3);

        job2.link(job3);
        job1.link(job2);
    }

    #[test]
    fn create_job() -> Result<()> {
        init();
        let input = "/usr/bin/touch".to_string();
        let _path = input.clone();
        let _argv: Vec<String> = input.split_whitespace().map(|s| s.to_string()).collect();
        let job = &mut Job::new(input, getpgrp());

        let process = Process::new("1".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));
        let process = Process::new("2".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        let pid = getpid();
        let pgid = getpgrp();
        let tmode = tcgetattr(SHELL_TERMINAL).expect("failed cgetattr");
        let _ctx = Context::new(pid, pgid, tmode, true);

        // info!("launch");

        Ok(())
    }

    #[test]
    fn is_stopped() {
        init();
        let input = "/usr/bin/touch";

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let process = Process::new("3".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(!is_job_stopped(job));

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(10));
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(is_job_stopped(job));
    }

    #[test]
    fn is_completed() {
        init();
        let input = "/usr/bin/touch";

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(0));
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(!is_job_completed(job));

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(is_job_completed(job));
    }
}
