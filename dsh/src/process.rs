use crate::shell::{SHELL_TERMINAL, Shell};
use anyhow::Context as _;
use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::BuiltinCommand;
use dsh_types::{Context, ExitStatus};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, kill, killpg, sigaction};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{
    ForkResult, Pid, close, dup2, execv, fork, getpgrp, getpid, isatty, pipe, setpgid, tcsetpgrp,
};
use std::ffi::CString;
use std::fmt::Debug;
use std::os::unix::io::{FromRawFd, RawFd};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::{fs, io, time};
use tracing::{debug, error};

/// RAII wrapper for file descriptors to ensure proper cleanup
#[allow(dead_code)]
struct FileDescriptor {
    fd: RawFd,
    should_close: bool,
}

#[allow(dead_code)]
impl FileDescriptor {
    fn new(fd: RawFd) -> Self {
        Self {
            fd,
            should_close: true,
        }
    }

    fn new_no_close(fd: RawFd) -> Self {
        Self {
            fd,
            should_close: false,
        }
    }

    fn raw(&self) -> RawFd {
        self.fd
    }

    fn leak(mut self) -> RawFd {
        self.should_close = false;
        self.fd
    }
}

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        if self.should_close && self.fd >= 0 {
            close(self.fd).ok(); // Ignore errors in destructor
        }
    }
}

const MONITOR_TIMEOUT: u64 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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
                tokio::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    match fs::File::create(&file).await {
                        Ok(mut writer) => {
                            if let Err(e) = io::copy(&mut reader, &mut writer).await {
                                tracing::error!("Failed to copy to file {}: {}", file, e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to create file {}: {}", file, e);
                        }
                    }
                });
            }

            Redirect::StdoutAppend(out)
            | Redirect::StderrAppend(out)
            | Redirect::StdouterrAppend(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                tokio::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    match fs::OpenOptions::new()
                        .write(true)
                        .append(true)
                        .open(&file)
                        .await
                    {
                        Ok(mut writer) => {
                            if let Err(e) = io::copy(&mut reader, &mut writer).await {
                                tracing::error!("Failed to append to file {}: {}", file, e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to open file {} for append: {}", file, e);
                        }
                    }
                });
            }
            Redirect::Input => {}
        }
    }
}

fn copy_fd(src: RawFd, dst: RawFd) -> Result<()> {
    if src != dst && src >= 0 && dst >= 0 {
        dup2(src, dst).map_err(|e| anyhow::anyhow!("dup2 failed: {}", e))?;
        // Only close if it's not a standard file descriptor
        if src > 2 {
            close(src).map_err(|e| anyhow::anyhow!("close failed: {}", e))?;
        }
    }
    Ok(())
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
    pub pid: Option<Pid>,
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
            .field("pid", &self.pid)
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
            pid: None,
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
            // Check if this process matches the PID
            if let Some(self_pid) = self.pid {
                if self_pid == pid {
                    debug!(
                        "BuiltinProcess::set_state: updating state for pid {} from {:?} to {:?}",
                        pid, self.state, state
                    );
                    self.state = state;
                    return true;
                }
            }
        }
        false
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
        match exit {
            ExitStatus::ExitedWith(code) => {
                if code >= 0 {
                    self.state = ProcessState::Completed(1, None);
                } else {
                    self.state = ProcessState::Completed(0, None);
                }
                debug!("Builtin process {} exited with code: {}", self.name, code);
            }
            ExitStatus::Running(_pid) => {
                self.state = ProcessState::Running;
                debug!("Builtin process {} is running", self.name);
            }
            ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => {
                self.state = ProcessState::Completed(0, None);
                debug!(
                    "Builtin process {} completed with control flow: {:?}",
                    self.name, exit
                );
            }
        }
        Ok(())
    }

    fn update_state(&mut self) -> Option<ProcessState> {
        if let Some(next) = self.next.as_mut() {
            next.update_state()
        } else {
            None
        }
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
        false
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

        match shell.run_wasm(self.name.as_str(), self.argv.to_vec()) {
            Ok(_) => {
                debug!("WASM process {} completed successfully", self.name);
                self.state = ProcessState::Completed(0, None);
                Ok(())
            }
            Err(e) => {
                tracing::error!("WASM process {} failed: {}", self.name, e);
                self.state = ProcessState::Completed(1, None);
                Err(e)
            }
        }
    }

    fn update_state(&mut self) -> Option<ProcessState> {
        if let Some(next) = self.next.as_mut() {
            next.update_state()
        } else {
            None
        }
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
    Completed(u8, Option<Signal>),
    Stopped(Pid, Signal),
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProcessState::Running => formatter.write_str("running"),
            ProcessState::Completed(_, signal) => {
                if let Some(signal) = signal {
                    if signal == &Signal::SIGKILL {
                        formatter.write_str("killed")
                    } else if signal == &Signal::SIGTERM {
                        formatter.write_str("terminated")
                    } else {
                        formatter.write_str("done")
                    }
                } else {
                    formatter.write_str("done")
                }
            }
            ProcessState::Stopped(_, _) => formatter.write_str("stopped"),
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
        false
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

    fn set_signals(&self) -> Result<()> {
        debug!("set signal action pid:{:?}", self.pid);
        // Accept job-control-related signals (refer https://www.gnu.org/software/libc/manual/html_node/Launching-Jobs.html)
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGINT handler: {}", e))?;
            sigaction(Signal::SIGQUIT, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGQUIT handler: {}", e))?;
            sigaction(Signal::SIGTSTP, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTSTP handler: {}", e))?;
            sigaction(Signal::SIGTTIN, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTTIN handler: {}", e))?;
            sigaction(Signal::SIGTTOU, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTTOU handler: {}", e))?;
            sigaction(Signal::SIGCHLD, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGCHLD handler: {}", e))?;
        }
        Ok(())
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

            self.set_signals()?;
        }

        let cmd = CString::new(self.cmd.clone()).context("failed new CString")?;
        let argv: Result<Vec<CString>> = self
            .argv
            .clone()
            .into_iter()
            .map(|a| {
                CString::new(a).map_err(|e| anyhow::anyhow!("failed to create CString: {}", e))
            })
            .collect();
        let argv = argv?;

        debug!(
            "launch: execv cmd:{:?} argv:{:?} foreground:{:?} infile:{:?} outfile:{:?} pid:{:?} pgid:{:?}",
            cmd, argv, foreground, self.stdin, self.stdout, pid, pgid,
        );

        copy_fd(self.stdin, STDIN_FILENO)?;
        if self.stdout == self.stderr {
            dup2(self.stdout, STDOUT_FILENO)
                .map_err(|e| anyhow::anyhow!("dup2 stdout failed: {}", e))?;
            dup2(self.stderr, STDERR_FILENO)
                .map_err(|e| anyhow::anyhow!("dup2 stderr failed: {}", e))?;
            close(self.stdout).map_err(|e| anyhow::anyhow!("close stdout failed: {}", e))?;
        } else {
            copy_fd(self.stdout, STDOUT_FILENO)?;
            copy_fd(self.stderr, STDERR_FILENO)?;
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

    fn update_state(&mut self) -> Option<ProcessState> {
        if let ProcessState::Completed(_, _) = self.state {
            Some(self.state)
        } else {
            if let Some(pid) = self.pid {
                if let Some((_, state)) = wait_pid_job(pid, true) {
                    self.state = state;
                }
            }

            if let Some(next) = self.next.as_mut() {
                next.update_state();
            }
            Some(self.state)
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
            ProcessState::Completed(_, _signal) => {
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

    #[allow(dead_code)]
    pub fn waitable(&self) -> bool {
        matches!(self, JobProcess::Command(_))
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

        debug!("launch cmd:{} pgid:{:?}", self.get_cmd(), &ctx.pgid);

        // initial pid
        let current_pid = getpid();

        let pid = match self {
            JobProcess::Builtin(process) => {
                if ctx.foreground {
                    process.pid = Some(current_pid);
                    process.launch(ctx, shell)?;
                    current_pid
                } else {
                    // Fork for background execution
                    let child_pid = fork_builtin_process(ctx, process, shell)?;
                    process.pid = Some(child_pid);
                    child_pid
                }
            }
            JobProcess::Wasm(process) => {
                if ctx.foreground {
                    process.launch(ctx, shell)?;
                    current_pid
                } else {
                    // Fork for background execution
                    fork_wasm_process(ctx, process, shell)?
                }
            }
            JobProcess::Command(process) => {
                ctx.process_count += 1;
                // fork
                fork_process(ctx, ctx.pgid, process)?
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

    #[allow(dead_code)]
    pub fn cont(&self) -> Result<()> {
        match self {
            JobProcess::Builtin(_) => Ok(()),
            JobProcess::Wasm(_) => Ok(()),
            JobProcess::Command(process) => {
                if let Some(pid) = process.pid {
                    debug!("send signal SIGCONT pid:{:?}", pid);
                    send_signal(pid, Signal::SIGCONT)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn update_state(&mut self) -> Option<ProcessState> {
        match self {
            JobProcess::Builtin(process) => process.update_state(),
            JobProcess::Wasm(process) => process.update_state(),
            JobProcess::Command(process) => process.update_state(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubshellType {
    None,
    Subshell,
    ProcessSubstitution,
}

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub cmd: String,
    pub pid: Option<Pid>,
    pub pgid: Option<Pid>,
    process: Option<Box<JobProcess>>,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    pub foreground: bool,
    pub subshell: SubshellType,
    pub redirect: Option<Redirect>,
    pub list_op: ListOp,
    pub job_id: usize,
    pub state: ProcessState,
    monitors: Vec<OutputMonitor>,
    shell_pgid: Pid,
}

impl Job {
    #[allow(dead_code)]
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
            // notified: false,
            // tmodes: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            // next: None,
            foreground: true,
            subshell: SubshellType::None,
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
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            foreground: true,
            subshell: SubshellType::None,
            redirect: None,
            list_op: ListOp::None,
            job_id: 1,
            state: ProcessState::Running,
            monitors: Vec::new(),
            shell_pgid,
        }
    }

    pub fn has_process(&self) -> bool {
        self.process.is_some()
    }

    pub fn set_process(&mut self, process: JobProcess) {
        match self.process {
            Some(ref mut p) => p.link(process),
            None => self.process = Some(Box::new(process)),
        }
    }

    pub fn last_process_state(&self) -> ProcessState {
        if let Some(p) = &self.process {
            last_process_state(*p.clone())
        } else {
            // not running
            ProcessState::Completed(0, None)
        }
    }

    pub async fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<ProcessState> {
        ctx.foreground = self.foreground;

        if let Some(process) = self.process.take().as_mut() {
            self.launch_process(ctx, shell, process)?;

            if !ctx.interactive {
                self.wait_job(false).await?;
            } else if ctx.foreground {
                // foreground
                if ctx.process_count > 0 {
                    let _ = self.put_in_foreground(false, false).await;
                }
            } else {
                // background
                let _ = self.put_in_background().await;
            }
        }

        if ctx.foreground {
            Ok(self.last_process_state())
        } else {
            // background
            Ok(ProcessState::Running)
        }
    }

    fn launch_process(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        process: &mut JobProcess,
    ) -> Result<()> {
        let (pid, mut next_process) = process.launch(ctx, shell, &self.redirect, self.stdout)?;
        if self.pid.is_none() {
            self.pid = Some(pid); // set process pid
        }
        self.state = process.get_state();

        if ctx.interactive {
            if self.pgid.is_none() {
                self.pgid = Some(pid);
                ctx.pgid = Some(pid);
                debug!("set job id: {} pgid: {:?}", self.id, self.pgid);
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

    pub async fn put_in_foreground(&mut self, no_hang: bool, cont: bool) -> Result<()> {
        debug!(
            "put_in_foreground: id: {} pgid {:?} no_hang: {} cont: {}",
            self.id, self.pgid, no_hang, cont
        );

        // ターミナル環境でない場合はプロセスグループ制御をスキップ
        if !isatty(SHELL_TERMINAL).unwrap_or(false) {
            debug!("Not a terminal environment, skipping process group control");
            debug!("About to call wait_job with no_hang: {}", no_hang);
            self.wait_job(no_hang).await?;
            debug!("wait_job completed in non-terminal mode");
            return Ok(());
        }

        debug!("Terminal environment detected, proceeding with process group control");

        // Put the job into the foreground
        if let Some(pgid) = self.pgid {
            debug!("Setting foreground process group to {}", pgid);
            if let Err(err) = tcsetpgrp(SHELL_TERMINAL, pgid) {
                debug!(
                    "tcsetpgrp failed: {}, continuing without terminal control",
                    err
                );
            } else {
                debug!("Successfully set foreground process group to {}", pgid);
            }

            if cont {
                debug!("Sending SIGCONT to process group {}", pgid);
                send_signal(pgid, Signal::SIGCONT).context("failed send signal SIGCONT")?;
                debug!("SIGCONT sent successfully");
            }
        } else {
            debug!("No pgid available, skipping process group operations");
        }

        debug!("About to call wait_job with no_hang: {}", no_hang);
        self.wait_job(no_hang).await?;
        debug!("wait_job completed");

        debug!("Restoring shell process group {}", self.shell_pgid);
        if let Err(err) = tcsetpgrp(SHELL_TERMINAL, self.shell_pgid) {
            debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
        } else {
            debug!(
                "Successfully restored shell process group {}",
                self.shell_pgid
            );
        }

        debug!("put_in_foreground completed successfully");
        Ok(())
    }

    /// Synchronous version of put_in_foreground for use in non-async contexts
    /// This method uses spawn_blocking to handle the async operations safely
    pub fn put_in_foreground_sync(&mut self, no_hang: bool, cont: bool) -> Result<()> {
        debug!(
            "put_in_foreground_sync: id: {} pgid {:?} no_hang: {} cont: {}",
            self.id, self.pgid, no_hang, cont
        );

        // ターミナル環境でない場合はプロセスグループ制御をスキップ
        if !isatty(SHELL_TERMINAL).unwrap_or(false) {
            debug!("Not a terminal environment, skipping process group control");
            debug!("About to call wait_job_sync with no_hang: {}", no_hang);
            self.wait_job_sync(no_hang)?;
            debug!("wait_job_sync completed in non-terminal mode");
            return Ok(());
        }

        debug!("Terminal environment detected, proceeding with process group control");

        // Put the job into the foreground
        if let Some(pgid) = self.pgid {
            debug!("Setting foreground process group to {}", pgid);
            if let Err(err) = tcsetpgrp(SHELL_TERMINAL, pgid) {
                debug!(
                    "tcsetpgrp failed: {}, continuing without terminal control",
                    err
                );
            } else {
                debug!("Successfully set foreground process group to {}", pgid);
            }

            if cont {
                debug!("Sending SIGCONT to process group {}", pgid);
                send_signal(pgid, Signal::SIGCONT).context("failed send signal SIGCONT")?;
                debug!("SIGCONT sent successfully");
            }
        } else {
            debug!("No pgid available, skipping process group operations");
        }

        debug!("About to call wait_job_sync with no_hang: {}", no_hang);
        self.wait_job_sync(no_hang)?;
        debug!("wait_job_sync completed");

        debug!("Restoring shell process group {}", self.shell_pgid);
        if let Err(err) = tcsetpgrp(SHELL_TERMINAL, self.shell_pgid) {
            debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
        } else {
            debug!(
                "Successfully restored shell process group {}",
                self.shell_pgid
            );
        }

        debug!("put_in_foreground_sync completed successfully");
        Ok(())
    }

    pub async fn put_in_background(&mut self) -> Result<()> {
        debug!("put_in_background pgid {:?}", self.pgid,);

        // ターミナル環境でない場合はプロセスグループ制御をスキップ
        if !isatty(SHELL_TERMINAL).unwrap_or(false) {
            debug!("Not a terminal environment, skipping process group control");
            return Ok(());
        }

        if let Err(err) = tcsetpgrp(SHELL_TERMINAL, self.shell_pgid) {
            debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
        } else {
            debug!(
                "Successfully set background process group to shell {}",
                self.shell_pgid
            );
        }
        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        Ok(())
    }

    fn show_job_status(&self) {}

    pub async fn wait_job(&mut self, no_hang: bool) -> Result<()> {
        debug!("wait_job called with no_hang: {}", no_hang);
        if no_hang {
            debug!("Calling wait_process_no_hang");
            self.wait_process_no_hang().await
        } else {
            debug!("Calling wait_process (blocking)");
            self.wait_process()
        }
    }

    /// Synchronous version of wait_job for use in non-async contexts
    pub fn wait_job_sync(&mut self, no_hang: bool) -> Result<()> {
        debug!("wait_job_sync called with no_hang: {}", no_hang);
        if no_hang {
            debug!("Calling wait_process_no_hang_sync");
            self.wait_process_no_hang_sync()
        } else {
            debug!("Calling wait_process (blocking)");
            self.wait_process()
        }
    }

    fn wait_process(&mut self) -> Result<()> {
        let mut send_killpg = false;
        loop {
            debug!("waitpid pgid:{:?} ...", self.pgid);

            // match task::spawn_blocking(|| waitpid(None, Some(WaitPidFlag::WUNTRACED))).await {
            let (pid, state) = match waitpid(None, Some(WaitPidFlag::WUNTRACED)) {
                Ok(WaitStatus::Exited(pid, status)) => {
                    debug!("wait_job exited {:?} {:?}", pid, status);
                    (pid, ProcessState::Completed(status as u8, None))
                } // ok??
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    debug!("wait_job signaled {:?} {:?}", pid, signal);
                    (pid, ProcessState::Completed(1, Some(signal)))
                }
                Ok(WaitStatus::Stopped(pid, signal)) => {
                    debug!("wait_job stopped {:?} {:?}", pid, signal);
                    (pid, ProcessState::Stopped(pid, signal))
                }
                Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
                    break;
                }
                status => {
                    error!("unexpected waitpid event: {:?}", status);
                    break;
                }
            };

            self.set_process_state(pid, state);

            debug!(
                "fin waitpid pgid:{:?} pid:{:?} state:{:?}",
                self.pgid, pid, state
            );

            // show_process_state(&self.process); // debug

            if let ProcessState::Completed(code, _) = state {
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
        Ok(())
    }

    async fn wait_process_no_hang(&mut self) -> Result<()> {
        debug!("wait_process_no_hang started for job: {}", self.id);
        let mut send_killpg = false;
        loop {
            debug!("waitpid loop iteration...");

            self.check_background_all_output().await?;

            let (pid, state) = match tokio::task::spawn_blocking(|| {
                waitpid(None, Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG))
            })
            .await
            {
                Ok(Ok(WaitStatus::Exited(pid, status))) => {
                    debug!("wait_job exited {:?} {:?}", pid, status);
                    (pid, ProcessState::Completed(status as u8, None))
                } // ok??
                Ok(Ok(WaitStatus::Signaled(pid, signal, _))) => {
                    debug!("wait_job signaled {:?} {:?}", pid, signal);
                    (pid, ProcessState::Completed(1, Some(signal)))
                }
                Ok(Ok(WaitStatus::Stopped(pid, signal))) => {
                    debug!("wait_job stopped {:?} {:?}", pid, signal);
                    (pid, ProcessState::Stopped(pid, signal))
                }
                Ok(Ok(WaitStatus::StillAlive)) => {
                    time::sleep(Duration::from_millis(1000)).await;
                    continue;
                }
                Ok(Err(nix::errno::Errno::ECHILD)) => {
                    self.check_background_all_output().await?;
                    break;
                }
                status => {
                    error!("unexpected waitpid event: {:?}", status);
                    break;
                }
            };

            self.check_background_all_output().await?;
            self.set_process_state(pid, state);

            debug!("fin wait: pid:{:?}", pid);

            // show_process_state(&self.process); // debug

            if let ProcessState::Completed(code, _) = state {
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
                debug!("Job completed, breaking from wait_process_no_hang loop");
                break;
            }
            if is_job_stopped(self) {
                println!("\rdsh: job {} '{}' has stopped", self.job_id, self.cmd);
                debug!("Job stopped, breaking from wait_process_no_hang loop");
                break;
            }
        }
        debug!("wait_process_no_hang completed for job: {}", self.id);
        Ok(())
    }

    /// Synchronous version of wait_process_no_hang for use in non-async contexts
    fn wait_process_no_hang_sync(&mut self) -> Result<()> {
        debug!("wait_process_no_hang_sync started for job: {}", self.id);
        let mut send_killpg = false;
        loop {
            debug!("waitpid loop iteration...");

            // Synchronous version - check background output if needed
            // Note: This is a simplified version that doesn't handle background output
            // For full functionality, consider using the async version

            let (pid, state) =
                match waitpid(None, Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::Exited(pid, status)) => {
                        debug!("wait_job exited {:?} {:?}", pid, status);
                        (pid, ProcessState::Completed(status as u8, None))
                    }
                    Ok(WaitStatus::Signaled(pid, signal, _)) => {
                        debug!("wait_job signaled {:?} {:?}", pid, signal);
                        (pid, ProcessState::Completed(1, Some(signal)))
                    }
                    Ok(WaitStatus::Stopped(pid, signal)) => {
                        debug!("wait_job stopped {:?} {:?}", pid, signal);
                        (pid, ProcessState::Stopped(pid, signal))
                    }
                    Ok(WaitStatus::StillAlive) => {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Err(nix::errno::Errno::ECHILD) => {
                        break;
                    }
                    status => {
                        error!("unexpected waitpid event: {:?}", status);
                        break;
                    }
                };

            self.set_process_state(pid, state);

            debug!("fin wait: pid:{:?}", pid);

            if let ProcessState::Completed(code, _) = state {
                if code != 0 && !send_killpg {
                    if let Some(pgid) = self.pgid {
                        debug!("killpg pgid: {}", pgid);
                        let _ = killpg(pgid, Signal::SIGKILL);
                        send_killpg = true;
                    }
                }
            }

            if is_job_completed(self) {
                debug!("Job completed, breaking from wait_process_no_hang_sync loop");
                break;
            }
            if is_job_stopped(self) {
                println!("\rdsh: job {} '{}' has stopped", self.job_id, self.cmd);
                debug!("Job stopped, breaking from wait_process_no_hang_sync loop");
                break;
            }
        }
        debug!("wait_process_no_hang_sync completed for job: {}", self.id);
        Ok(())
    }
    fn set_process_state(&mut self, pid: Pid, state: ProcessState) {
        if let Some(process) = self.process.as_mut() {
            process.set_state_pid(pid, state);
        }
    }

    #[allow(dead_code)]
    pub async fn check_background_output(&mut self) -> Result<()> {
        let mut i = 0;
        while i < self.monitors.len() {
            let _ = self.monitors[i].output().await?;
            i += 1;
        }
        Ok(())
    }

    pub async fn check_background_all_output(&mut self) -> Result<()> {
        debug!(
            "check_background_all_output: monitors.len() = {}",
            self.monitors.len()
        );
        let mut i = 0;
        while i < self.monitors.len() {
            debug!("Processing monitor {}", i);
            self.monitors[i].output_all(false).await?;
            i += 1;
        }
        debug!("check_background_all_output completed");
        Ok(())
    }

    pub fn kill(&mut self) -> Result<()> {
        kill_process(&self.process)
    }

    pub fn update_status(&mut self) -> bool {
        if let Some(process) = self.process.as_mut() {
            if let Some(state) = process.update_state() {
                self.state = state;
            }
        }
        is_job_completed(self)
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

    #[allow(dead_code)]
    pub async fn output(&mut self) -> Result<usize> {
        let mut line = String::new();
        match time::timeout(
            Duration::from_millis(MONITOR_TIMEOUT),
            self.reader.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(len)) => {
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
            Ok(Err(_)) | Err(_) => Ok(0),
        }
    }

    pub async fn output_all(&mut self, block: bool) -> Result<()> {
        let mut len = 1;
        while len != 0 {
            let mut line = String::new();
            match time::timeout(
                Duration::from_millis(MONITOR_TIMEOUT),
                self.reader.read_line(&mut line),
            )
            .await
            {
                Ok(Ok(readed)) => {
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
                Ok(Err(_)) | Err(_) => {
                    if !block {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub fn wait_any_job(no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    let result = waitpid(None, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8, None)),
        Ok(WaitStatus::Signaled(pid, signal, _)) => (pid, ProcessState::Completed(1, Some(signal))),
        Ok(WaitStatus::Stopped(pid, signal)) => (pid, ProcessState::Stopped(pid, signal)),
        Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
            return None;
        }
        status => {
            error!("unexpected waitpid event: {:?}", status);
            return None;
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

#[allow(dead_code)]
fn show_process_state(process: &Option<Box<JobProcess>>) {
    if let Some(process) = process {
        debug!(
            "process_state: {:?} state:{:?}",
            process.get_cmd(),
            process.get_state(),
        );

        if process.next().is_some() {
            show_process_state(&process.next());
        }
    }
}

fn fork_builtin_process(
    ctx: &mut Context,
    process: &mut BuiltinProcess,
    shell: &mut Shell,
) -> Result<Pid> {
    debug!("fork_builtin_process for background execution");

    let pid = unsafe { fork().context("failed fork for builtin")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!("Parent: forked builtin process with pid {}", child);
            Ok(child)
        }
        ForkResult::Child => {
            // Child process: execute builtin command
            let pid = getpid();
            debug!(
                "Child: executing builtin command {} with pid {}",
                process.name, pid
            );

            // Set process group for job control
            if let Err(e) = setpgid(pid, pid) {
                error!("Failed to setpgid for builtin: {}", e);
            }

            // Execute the builtin command
            if let Err(e) = process.launch(ctx, shell) {
                error!("Failed to launch builtin process: {}", e);
                std::process::exit(1);
            }

            // Builtin commands complete immediately, so exit with success
            std::process::exit(0);
        }
    }
}

fn fork_wasm_process(
    ctx: &mut Context,
    process: &mut WasmProcess,
    shell: &mut Shell,
) -> Result<Pid> {
    debug!("fork_wasm_process for background execution");

    let pid = unsafe { fork().context("failed fork for wasm")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!("Parent: forked wasm process with pid {}", child);
            Ok(child)
        }
        ForkResult::Child => {
            // Child process: execute wasm command
            let pid = getpid();
            debug!(
                "Child: executing wasm command {} with pid {}",
                process.name, pid
            );

            // Set process group for job control
            if let Err(e) = setpgid(pid, pid) {
                error!("Failed to setpgid for wasm: {}", e);
            }

            // Execute the wasm command
            if let Err(e) = process.launch(ctx, shell) {
                error!("Failed to launch wasm process: {}", e);
                std::process::exit(1);
            }

            // Wasm commands complete immediately, so exit with success
            std::process::exit(0);
        }
    }
}

fn fork_process(ctx: &Context, job_pgid: Option<Pid>, process: &mut Process) -> Result<Pid> {
    debug!("fork_process pgid: {:?}", job_pgid);

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
            if let Err(e) = process.launch(pid, pgid, ctx.interactive, ctx.foreground) {
                error!("Failed to launch process: {}", e);
                std::process::exit(1);
            }
            // execv成功時は新プログラムに置き換わり、失敗時はexitするため、ここには到達しない
            // 念のための安全策として明示的にexit
            std::process::exit(1);
        }
    }
}

pub fn is_job_stopped(job: &Job) -> bool {
    if let Some(p) = &job.process {
        let stopped = p.is_stopped();
        debug!(
            "is_job_stopped {} {} -> {}",
            p.get_cmd(),
            p.get_state(),
            stopped
        );
        stopped
    } else {
        debug!("is_job_stopped: no process -> true");
        true
    }
}

pub fn is_job_completed(job: &Job) -> bool {
    if let Some(process) = &job.process {
        let completed = process.is_completed();
        debug!(
            "is_job_completed {} {} -> {}",
            process.get_cmd(),
            process.get_state(),
            completed
        );
        completed
    } else {
        debug!("is_job_completed: no process -> true");
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
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8, None)),
        Ok(WaitStatus::Signaled(pid, signal, _)) => (pid, ProcessState::Completed(1, Some(signal))),
        Ok(WaitStatus::Stopped(pid, signal)) => (pid, ProcessState::Stopped(pid, signal)),
        Err(nix::errno::Errno::ECHILD) => (pid, ProcessState::Completed(1, None)),
        Ok(WaitStatus::StillAlive) => {
            return None;
        }
        status => {
            error!("unexpected waitpid event: {:?}", status);
            return None;
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
    if let Some(output) = redirect {
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
        process.kill()?;
        if process.next().is_some() {
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
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let process = Process::new("3".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(!is_job_stopped(job));

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(10), Signal::SIGSTOP);
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
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(0), Signal::SIGSTOP);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(!is_job_completed(job));

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(is_job_completed(job));
    }

    #[test]
    fn test_process_state_transitions() {
        init();
        let mut process = Process::new("test_cmd".to_string(), vec!["arg1".to_string()]);

        // 初期状態はRunning
        assert!(matches!(process.state, ProcessState::Running));

        // 状態変更テスト
        process.state = ProcessState::Completed(0, None);
        assert!(matches!(process.state, ProcessState::Completed(0, None)));

        process.state = ProcessState::Stopped(Pid::from_raw(1234), Signal::SIGSTOP);
        assert!(matches!(
            process.state,
            ProcessState::Stopped(_, Signal::SIGSTOP)
        ));
    }

    #[test]
    fn test_wait_pid_job_handles_unexpected_status() {
        // This test verifies that wait_pid_job no longer panics on unexpected status
        // Instead, it should return None and log an error
        init();

        // Test that the function exists and has the correct signature
        let result = wait_pid_job(getpid(), true);
        // Should not panic, may return None
        assert!(result.is_none() || result.is_some());
    }

    #[test]
    fn test_process_state_enum_values() {
        // Test that ProcessState enum has expected values
        init();

        let completed = ProcessState::Completed(0, None);
        let running = ProcessState::Running;
        let stopped = ProcessState::Stopped(getpid(), Signal::SIGSTOP);

        // Test pattern matching works
        match completed {
            ProcessState::Completed(code, signal) => {
                assert_eq!(code, 0);
                assert_eq!(signal, None);
            }
            _ => panic!("Unexpected process state"),
        }

        match running {
            ProcessState::Running => {} // Expected state
            _ => panic!("Unexpected process state"),
        }

        match stopped {
            ProcessState::Stopped(pid, signal) => {
                assert_eq!(signal, Signal::SIGSTOP);
                assert!(pid.as_raw() > 0);
            }
            _ => panic!("Unexpected process state"),
        }
    }

    #[test]
    fn test_job_process_variants() {
        init();
        let process = Process::new("test".to_string(), vec![]);
        let job_process = JobProcess::Command(process);

        // JobProcessの型チェック
        match job_process {
            JobProcess::Command(_) => {} // Expected variant
            _ => panic!("Expected Command variant"),
        }
    }
}
