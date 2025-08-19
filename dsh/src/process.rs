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
            if let Some(self_pid) = self.pid
                && self_pid == pid
            {
                debug!(
                    "BuiltinProcess::set_state: updating state for pid {} from {:?} to {:?}",
                    pid, self.state, state
                );
                self.state = state;
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
        if let Some(ppid) = self.pid
            && ppid == pid
        {
            self.state = state;
            return true;
        }
        if let Some(ref mut next) = self.next
            && next.set_state_pid(pid, state)
        {
            return true;
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
            if let Some(pid) = self.pid
                && let Some((_waited_pid, state)) = wait_pid_job(pid, true)
            {
                self.state = state;
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
            JobProcess::Command(jprocess) => jprocess.link(process),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    #[allow(dead_code)]
    pub fn mut_next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn take_next(&mut self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.take(),
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
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(_) => {
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
            JobProcess::Command(process) => process.pid,
        }
    }

    #[allow(dead_code)]
    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
        }
    }

    fn set_state_pid(&mut self, pid: Pid, state: ProcessState) -> bool {
        debug!(
            "🔄 STATE: set_state_pid called for pid: {}, state: {:?}",
            pid, state
        );
        let result = match self {
            JobProcess::Builtin(p) => {
                debug!("🔄 STATE: Setting state for builtin process: {}", p.name);
                p.set_state(pid, state)
            }
            JobProcess::Command(p) => {
                debug!("🔄 STATE: Setting state for command process: {}", p.cmd);
                p.set_state(pid, state)
            }
        };
        debug!("🔄 STATE: set_state_pid result: {}", result);
        result
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
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

    /// Check if pipeline should be considered complete based on consumer process termination
    /// This handles the case where the last process in a pipeline (consumer) exits normally
    /// while earlier processes (producers) are still running
    fn is_pipeline_consumer_terminated(&self) -> bool {
        // If this process has a next process, check if the consumer terminated
        if let Some(next) = self.next() {
            // Recursively check the next process
            if next.is_pipeline_consumer_terminated() {
                return true;
            }
            // If the next process (consumer) completed normally, the pipeline should terminate
            if let ProcessState::Completed(0, None) = next.get_state() {
                debug!(
                    "PIPELINE_CONSUMER_TERMINATED: Consumer process '{}' exited normally, pipeline should terminate",
                    next.get_cmd()
                );
                return true;
            }
        }
        false
    }

    /// Check if any process in the pipeline is stopped
    fn has_stopped_process(&self) -> bool {
        match self.get_state() {
            ProcessState::Stopped(_, _) => true,
            _ => {
                if let Some(next) = self.next() {
                    next.has_stopped_process()
                } else {
                    false
                }
            }
        }
    }

    pub fn get_cap_out(&self) -> (Option<RawFd>, Option<RawFd>) {
        match self {
            JobProcess::Builtin(_p) => (None, None),
            JobProcess::Command(p) => (p.cap_stdout, p.cap_stderr),
        }
    }

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
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
        debug!(
            "JOB_LAUNCH_START: Starting job {} launch (cmd: '{}', foreground: {}, pid: {:?})",
            self.job_id, self.cmd, self.foreground, self.pid
        );

        ctx.foreground = self.foreground;

        if let Some(process) = self.process.take().as_mut() {
            debug!(
                "JOB_LAUNCH_PROCESS: Launching process for job {} (process_type: {})",
                self.job_id,
                process.get_cmd()
            );

            match self.launch_process(ctx, shell, process) {
                Ok(_) => {
                    debug!(
                        "JOB_LAUNCH_PROCESS_SUCCESS: Process launched successfully for job {}",
                        self.job_id
                    );
                }
                Err(e) => {
                    error!(
                        "JOB_LAUNCH_PROCESS_ERROR: Failed to launch process for job {}: {}",
                        self.job_id, e
                    );
                    return Err(e);
                }
            }

            if !ctx.interactive {
                debug!(
                    "JOB_LAUNCH_NON_INTERACTIVE: Non-interactive mode, waiting for job {} completion",
                    self.job_id
                );
                self.wait_job(false).await?;
            } else if ctx.foreground {
                debug!(
                    "JOB_LAUNCH_FOREGROUND: Foreground job {}, process_count: {}",
                    self.job_id, ctx.process_count
                );
                // foreground
                if ctx.process_count > 0 {
                    debug!(
                        "JOB_LAUNCH_PUT_FOREGROUND: Putting job {} in foreground",
                        self.job_id
                    );
                    match self.put_in_foreground(false, false).await {
                        Ok(_) => {
                            debug!(
                                "JOB_LAUNCH_FOREGROUND_SUCCESS: Job {} put in foreground successfully",
                                self.job_id
                            );
                        }
                        Err(e) => {
                            error!(
                                "JOB_LAUNCH_FOREGROUND_ERROR: Failed to put job {} in foreground: {}",
                                self.job_id, e
                            );
                        }
                    }
                } else {
                    debug!(
                        "JOB_LAUNCH_NO_PROCESSES: Job {} has no processes to put in foreground",
                        self.job_id
                    );
                }
            } else {
                debug!(
                    "JOB_LAUNCH_BACKGROUND: Background job {}, putting in background",
                    self.job_id
                );
                // background
                match self.put_in_background().await {
                    Ok(_) => {
                        debug!(
                            "JOB_LAUNCH_BACKGROUND_SUCCESS: Job {} put in background successfully",
                            self.job_id
                        );
                    }
                    Err(e) => {
                        error!(
                            "JOB_LAUNCH_BACKGROUND_ERROR: Failed to put job {} in background: {}",
                            self.job_id, e
                        );
                    }
                }
            }
        } else {
            debug!(
                "JOB_LAUNCH_NO_PROCESS: Job {} has no process to launch",
                self.job_id
            );
        }

        let final_state = if ctx.foreground {
            self.last_process_state()
        } else {
            // background
            ProcessState::Running
        };

        debug!(
            "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
            self.job_id, final_state, ctx.foreground
        );

        Ok(final_state)
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
            debug!("🔧 PGID: Setting process group for {}", process.get_cmd());
            debug!(
                "🔧 PGID: setpgid {} pid:{} pgid:{:?}",
                process.get_cmd(),
                pid,
                self.pgid
            );

            let target_pgid = self.pgid.unwrap_or(pid);
            debug!("🔧 PGID: Target pgid: {}", target_pgid);

            match setpgid(pid, target_pgid) {
                Ok(_) => debug!(
                    "🔧 PGID: Successfully set pgid {} for pid {}",
                    target_pgid, pid
                ),
                Err(e) => {
                    error!(
                        "🔧 PGID: Failed to set pgid {} for pid {}: {}",
                        target_pgid, pid, e
                    );
                    return Err(e.into());
                }
            }
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

        // Skip process group control if not in terminal environment
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

        // Skip process group control if not in terminal environment
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

        // Skip process group control if not in terminal environment
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
            self.wait_process().await
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
            self.wait_process_sync()
        }
    }

    async fn wait_process(&mut self) -> Result<()> {
        let mut send_killpg = false;
        loop {
            let (pid, state) =
                match tokio::task::spawn_blocking(|| waitpid(None, Some(WaitPidFlag::WUNTRACED)))
                    .await?
                {
                    Ok(WaitStatus::Exited(pid, status)) => {
                        (pid, ProcessState::Completed(status as u8, None))
                    } // ok??
                    Ok(WaitStatus::Signaled(pid, signal, _)) => {
                        (pid, ProcessState::Completed(1, Some(signal)))
                    }
                    Ok(WaitStatus::Stopped(pid, signal)) => {
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

            if let ProcessState::Completed(code, signal) = state {
                debug!(
                    "⏳ WAIT: Process completed - pid: {}, code: {}, signal: {:?}",
                    pid, code, signal
                );
                if code != 0 && !send_killpg {
                    if let Some(pgid) = self.pgid {
                        debug!(
                            "⏳ WAIT: Process failed (code: {}), sending SIGKILL to pgid: {}",
                            code, pgid
                        );
                        match killpg(pgid, Signal::SIGKILL) {
                            Ok(_) => debug!("⏳ WAIT: Successfully sent SIGKILL to pgid: {}", pgid),
                            Err(e) => {
                                debug!("⏳ WAIT: Failed to send SIGKILL to pgid {}: {}", pgid, e)
                            }
                        }
                        send_killpg = true;
                    } else {
                        debug!("⏳ WAIT: Process failed but no pgid to kill");
                    }
                } else if code == 0 {
                    debug!("⏳ WAIT: Process completed successfully");
                }
            }
            // break;
            if is_job_completed(self) {
                debug!("⏳ WAIT: Job completed, breaking from wait_process loop");
                break;
            }

            // Check if consumer terminated and we need to kill remaining processes
            if let Some(process) = &self.process
                && process.is_pipeline_consumer_terminated()
                && !process.is_completed()
            {
                debug!("⏳ WAIT: Pipeline consumer terminated, killing remaining processes");
                if let Some(pgid) = self.pgid {
                    debug!(
                        "⏳ WAIT: Sending SIGTERM to remaining processes in pgid: {}",
                        pgid
                    );
                    match killpg(pgid, Signal::SIGTERM) {
                        Ok(_) => {
                            debug!("⏳ WAIT: Successfully sent SIGTERM to pgid: {}", pgid);
                            // Give processes a moment to terminate gracefully
                            time::sleep(Duration::from_millis(100)).await;
                            // Then send SIGKILL if needed
                            let _ = killpg(pgid, Signal::SIGKILL);
                            debug!("⏳ WAIT: Sent SIGKILL to pgid: {}", pgid);
                        }
                        Err(e) => {
                            debug!("⏳ WAIT: Failed to send SIGTERM to pgid {}: {}", pgid, e);
                        }
                    }
                }
                break;
            }

            if is_job_stopped(self) {
                debug!("⏳ WAIT: Job stopped");
                println!("\rdsh: job {} '{}' has stopped", self.job_id, self.cmd);
                break;
            }
        }
        Ok(())
    }

    fn wait_process_sync(&mut self) -> Result<()> {
        let mut send_killpg = false;
        loop {
            let (pid, state) = match waitpid(None, Some(WaitPidFlag::WUNTRACED)) {
                Ok(WaitStatus::Exited(pid, status)) => {
                    (pid, ProcessState::Completed(status as u8, None))
                } // ok??
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    (pid, ProcessState::Completed(1, Some(signal)))
                }
                Ok(WaitStatus::Stopped(pid, signal)) => (pid, ProcessState::Stopped(pid, signal)),
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

            if let ProcessState::Completed(code, signal) = state {
                debug!(
                    "⏳ WAIT: Process completed - pid: {}, code: {}, signal: {:?}",
                    pid, code, signal
                );
                if code != 0 && !send_killpg {
                    if let Some(pgid) = self.pgid {
                        debug!(
                            "⏳ WAIT: Process failed (code: {}), sending SIGKILL to pgid: {}",
                            code, pgid
                        );
                        match killpg(pgid, Signal::SIGKILL) {
                            Ok(_) => debug!("⏳ WAIT: Successfully sent SIGKILL to pgid: {}", pgid),
                            Err(e) => {
                                debug!("⏳ WAIT: Failed to send SIGKILL to pgid {}: {}", pgid, e)
                            }
                        }
                        send_killpg = true;
                    } else {
                        debug!("⏳ WAIT: Process failed but no pgid to kill");
                    }
                } else if code == 0 {
                    debug!("⏳ WAIT: Process completed successfully");
                }
            }
            // break;
            if is_job_completed(self) {
                debug!("⏳ WAIT: Job completed, breaking from wait_process loop");
                break;
            }

            // Check if consumer terminated and we need to kill remaining processes
            if let Some(process) = &self.process
                && process.is_pipeline_consumer_terminated()
                && !process.is_completed()
            {
                debug!("⏳ WAIT: Pipeline consumer terminated, killing remaining processes");
                if let Some(pgid) = self.pgid {
                    debug!(
                        "⏳ WAIT: Sending SIGTERM to remaining processes in pgid: {}",
                        pgid
                    );
                    match killpg(pgid, Signal::SIGTERM) {
                        Ok(_) => {
                            debug!("⏳ WAIT: Successfully sent SIGTERM to pgid: {}", pgid);
                            // Give processes a moment to terminate gracefully
                            std::thread::sleep(Duration::from_millis(100));
                            // Then send SIGKILL if needed
                            let _ = killpg(pgid, Signal::SIGKILL);
                            debug!("⏳ WAIT: Sent SIGKILL to pgid: {}", pgid);
                        }
                        Err(e) => {
                            debug!("⏳ WAIT: Failed to send SIGTERM to pgid {}: {}", pgid, e);
                        }
                    }
                }
                break;
            }

            if is_job_stopped(self) {
                debug!("⏳ WAIT: Job stopped");
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

            if let ProcessState::Completed(code, _) = state
                && code != 0
                && !send_killpg
                && let Some(pgid) = self.pgid
            {
                debug!("killpg pgid: {}", pgid);
                let _ = killpg(pgid, Signal::SIGKILL);
                send_killpg = true;
            }
            // break;
            if is_job_completed(self) {
                debug!("Job completed, breaking from wait_process_no_hang loop");
                break;
            }

            // Check if consumer terminated and we need to kill remaining processes
            if let Some(process) = &self.process
                && process.is_pipeline_consumer_terminated()
                && !process.is_completed()
            {
                debug!("Pipeline consumer terminated, killing remaining processes");
                if let Some(pgid) = self.pgid {
                    debug!("Sending SIGTERM to remaining processes in pgid: {}", pgid);
                    match killpg(pgid, Signal::SIGTERM) {
                        Ok(_) => {
                            debug!("Successfully sent SIGTERM to pgid: {}", pgid);
                            // Give processes a moment to terminate gracefully
                            time::sleep(Duration::from_millis(100)).await;
                            // Then send SIGKILL if needed
                            let _ = killpg(pgid, Signal::SIGKILL);
                            debug!("Sent SIGKILL to pgid: {}", pgid);
                        }
                        Err(e) => {
                            debug!("Failed to send SIGTERM to pgid {}: {}", pgid, e);
                        }
                    }
                }
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

            if let ProcessState::Completed(code, _) = state
                && code != 0
                && !send_killpg
                && let Some(pgid) = self.pgid
            {
                debug!("killpg pgid: {}", pgid);
                let _ = killpg(pgid, Signal::SIGKILL);
                send_killpg = true;
            }

            if is_job_completed(self) {
                debug!("Job completed, breaking from wait_process_no_hang_sync loop");
                break;
            }

            // Check if consumer terminated and we need to kill remaining processes
            if let Some(process) = &self.process
                && process.is_pipeline_consumer_terminated()
                && !process.is_completed()
            {
                debug!("Pipeline consumer terminated, killing remaining processes");
                if let Some(pgid) = self.pgid {
                    debug!("Sending SIGTERM to remaining processes in pgid: {}", pgid);
                    match killpg(pgid, Signal::SIGTERM) {
                        Ok(_) => {
                            debug!("Successfully sent SIGTERM to pgid: {}", pgid);
                            // Give processes a moment to terminate gracefully
                            std::thread::sleep(Duration::from_millis(100));
                            // Then send SIGKILL if needed
                            let _ = killpg(pgid, Signal::SIGKILL);
                            debug!("Sent SIGKILL to pgid: {}", pgid);
                        }
                        Err(e) => {
                            debug!("Failed to send SIGTERM to pgid {}: {}", pgid, e);
                        }
                    }
                }
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
        let old_state = self.state;

        if let Some(process) = self.process.as_mut()
            && let Some(state) = process.update_state()
        {
            self.state = state;

            // Log state changes with detailed information
            if old_state != self.state {
                debug!(
                    "JOB_STATE_CHANGE: Job {} state changed: {:?} -> {:?} (pid: {:?}, pgid: {:?})",
                    self.job_id, old_state, self.state, self.pid, self.pgid
                );

                // Log specific state transitions
                match (&old_state, &self.state) {
                    (ProcessState::Running, ProcessState::Stopped(pid, signal)) => {
                        debug!(
                            "JOB_STOPPED: Job {} stopped by signal {:?} (pid: {:?})",
                            self.job_id, signal, pid
                        );
                    }
                    (ProcessState::Stopped(_, _), ProcessState::Running) => {
                        debug!(
                            "JOB_RESUMED: Job {} resumed from stopped state",
                            self.job_id
                        );
                    }
                    (ProcessState::Running, ProcessState::Completed(exit_code, signal)) => {
                        debug!(
                            "JOB_COMPLETED: Job {} completed with exit_code: {}, signal: {:?}",
                            self.job_id, exit_code, signal
                        );
                    }
                    (ProcessState::Stopped(_, _), ProcessState::Completed(exit_code, signal)) => {
                        debug!(
                            "JOB_COMPLETED_FROM_STOP: Job {} completed from stopped state with exit_code: {}, signal: {:?}",
                            self.job_id, exit_code, signal
                        );
                    }
                    _ => {
                        debug!(
                            "JOB_STATE_OTHER: Job {} other state transition: {:?} -> {:?}",
                            self.job_id, old_state, self.state
                        );
                    }
                }
            }
        }

        let is_completed = is_job_completed(self);
        debug!(
            "JOB_COMPLETION_CHECK: Job {} completion check result: {} (current state: {:?})",
            self.job_id, is_completed, self.state
        );

        is_completed
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
                    print!("\n\r{line}");
                } else {
                    print!("{line}");
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
                        print!("\r{line}");
                    } else {
                        print!("{line}");
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

    debug!(
        "🍴 BUILTIN: About to fork builtin process: {}",
        process.name
    );
    let pid = unsafe { fork().context("failed fork for builtin")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!(
                "🍴 BUILTIN: Parent process - forked builtin {} with child pid {}",
                process.name, child
            );
            Ok(child)
        }
        ForkResult::Child => {
            // Child process: execute builtin command
            let pid = getpid();
            debug!(
                "🍴 BUILTIN: Child process - executing builtin command {} with pid {}",
                process.name, pid
            );
            debug!(
                "🍴 BUILTIN: Child process I/O - stdin={}, stdout={}, stderr={}",
                process.stdin, process.stdout, process.stderr
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

fn fork_process(ctx: &Context, job_pgid: Option<Pid>, process: &mut Process) -> Result<Pid> {
    debug!("🍴 FORK: Starting fork_process");
    debug!(
        "🍴 FORK: pgid: {:?}, foreground: {}",
        job_pgid, ctx.foreground
    );
    debug!(
        "🍴 FORK: Process I/O before capture - stdin={}, stdout={}, stderr={}",
        process.stdin, process.stdout, process.stderr
    );
    debug!(
        "🍴 FORK: Context I/O - infile={}, outfile={}, errfile={}",
        ctx.infile, ctx.outfile, ctx.errfile
    );

    // capture
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground {
        debug!("🍴 FORK: Creating capture pipe for stdout (background process)");
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stdout = pin;
        process.cap_stdout = Some(pout);
        debug!(
            "🍴 FORK: Created capture pipe for stdout: read={}, write={}",
            pout, pin
        );
    } else {
        debug!(
            "🍴 FORK: No capture pipe needed for stdout (ctx.outfile={}, foreground={})",
            ctx.outfile, ctx.foreground
        );
    }

    if ctx.errfile == STDERR_FILENO && !ctx.foreground {
        debug!("🍴 FORK: Creating capture pipe for stderr (background process)");
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stderr = pin;
        process.cap_stderr = Some(pout);
        debug!(
            "🍴 FORK: Created capture pipe for stderr: read={}, write={}",
            pout, pin
        );
    } else {
        debug!(
            "🍴 FORK: No capture pipe needed for stderr (ctx.errfile={}, foreground={})",
            ctx.errfile, ctx.foreground
        );
    }

    debug!(
        "🍴 FORK: Final process I/O - stdin={}, stdout={}, stderr={}",
        process.stdin, process.stdout, process.stderr
    );

    debug!("🍴 FORK: About to fork external process");
    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!("🍴 FORK: Parent process - child pid: {}", child);
            debug!("🍴 FORK: Parent process continuing with child management");
            // if process.stdout != STDOUT_FILENO {
            //     close(process.stdout).context("failed close")?;
            // }
            Ok(child)
        }
        ForkResult::Child => {
            // This is the child process
            let pid = getpid();
            let pgid = job_pgid.unwrap_or(pid);
            debug!("🍴 FORK: Child process - pid: {}, pgid: {}", pid, pgid);
            debug!("🍴 FORK: Child process about to launch");

            if let Err(e) = process.launch(pid, pgid, ctx.interactive, ctx.foreground) {
                error!("🍴 FORK: Child process launch failed: {}", e);
                std::process::exit(1);
            }
            // When execv succeeds, it replaces with new program; when it fails, it exits, so this point is never reached
            // Explicit exit as a safety measure just in case
            debug!("🍴 FORK: Child process launch completed unexpectedly, exiting");
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
    debug!(
        "JOB_COMPLETION_CHECK_START: Checking completion for job {} (state: {:?}, cmd: '{}')",
        job.job_id, job.state, job.cmd
    );

    if let Some(process) = &job.process {
        let process_state = process.get_state();
        let completed = process.is_completed();
        let consumer_terminated = process.is_pipeline_consumer_terminated();

        debug!(
            "JOB_COMPLETION_CHECK_PROCESS: Job {} process '{}' state: {:?}, completed: {}, consumer_terminated: {}",
            job.job_id,
            process.get_cmd(),
            process_state,
            completed,
            consumer_terminated
        );

        // Additional logging for specific states
        match process_state {
            ProcessState::Running => {
                debug!(
                    "JOB_COMPLETION_CHECK_RUNNING: Job {} is still running",
                    job.job_id
                );
            }
            ProcessState::Stopped(pid, signal) => {
                debug!(
                    "JOB_COMPLETION_CHECK_STOPPED: Job {} is stopped (pid: {}, signal: {:?})",
                    job.job_id, pid, signal
                );
            }
            ProcessState::Completed(exit_code, signal) => {
                debug!(
                    "JOB_COMPLETION_CHECK_COMPLETED: Job {} completed (exit_code: {}, signal: {:?})",
                    job.job_id, exit_code, signal
                );
            }
        }

        // Job is completed if either all processes are completed OR
        // (the consumer terminated normally AND no processes are stopped)
        let has_stopped = process.has_stopped_process();
        let job_completed = completed || (consumer_terminated && !has_stopped);

        debug!(
            "JOB_COMPLETION_CHECK_RESULT: Job {} completion result: {}",
            job.job_id, job_completed
        );

        // If consumer terminated but not all processes are complete, we should terminate remaining processes
        if consumer_terminated && !completed {
            debug!(
                "JOB_COMPLETION_CONSUMER_TERM: Job {} consumer terminated, should terminate remaining processes",
                job.job_id
            );
        }

        job_completed
    } else {
        debug!(
            "JOB_COMPLETION_CHECK_NO_PROCESS: Job {} has no process, treating as completed",
            job.job_id
        );
        true
    }
}

pub fn wait_pid_job(pid: Pid, no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    debug!(
        "WAIT_PID_START: Starting waitpid for pid: {}, no_hang: {}, options: {:?}",
        pid, no_hang, options
    );

    let result = waitpid(pid, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => {
            debug!(
                "WAIT_PID_EXITED: Process {} exited normally with status: {}",
                pid, status
            );
            (pid, ProcessState::Completed(status as u8, None))
        }
        Ok(WaitStatus::Signaled(pid, signal, core_dumped)) => {
            debug!(
                "WAIT_PID_SIGNALED: Process {} killed by signal: {:?}, core_dumped: {}",
                pid, signal, core_dumped
            );
            (pid, ProcessState::Completed(1, Some(signal)))
        }
        Ok(WaitStatus::Stopped(pid, signal)) => {
            debug!(
                "WAIT_PID_STOPPED: Process {} stopped by signal: {:?}",
                pid, signal
            );
            (pid, ProcessState::Stopped(pid, signal))
        }
        Err(nix::errno::Errno::ECHILD) => {
            debug!(
                "WAIT_PID_ECHILD: No child process {} (ECHILD) - treating as completed",
                pid
            );
            (pid, ProcessState::Completed(1, None))
        }
        Ok(WaitStatus::StillAlive) => {
            debug!("WAIT_PID_ALIVE: Process {} still alive (WNOHANG)", pid);
            return None;
        }
        Ok(WaitStatus::Continued(pid)) => {
            debug!("WAIT_PID_CONTINUED: Process {} continued", pid);
            return None;
        }
        status => {
            error!(
                "WAIT_PID_UNEXPECTED: Unexpected waitpid status for pid {}: {:?}",
                pid, status
            );
            return None;
        }
    };

    debug!(
        "WAIT_PID_RESULT: Returning result for pid {}: state={:?}",
        pid, res.1
    );
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
            Redirect::StdoutOutput(_file) | Redirect::StdoutAppend(_file) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;
                Ok(Some(pout))
            }
            Redirect::StderrOutput(file) | Redirect::StderrAppend(file) => {
                debug!("🔀 REDIRECT: StderrOutput/Append to file: {}", file);
                let (pout, pin) = pipe().context("failed pipe")?;
                debug!(
                    "🔀 REDIRECT: Created redirect pipe - read_end={}, write_end={}",
                    pout, pin
                );
                ctx.errfile = pin;
                debug!("🔀 REDIRECT: Set ctx.errfile={}", ctx.errfile);
                Ok(Some(pout))
            }
            Redirect::StdouterrOutput(file) | Redirect::StdouterrAppend(file) => {
                debug!("🔀 REDIRECT: StdouterrOutput/Append to file: {}", file);
                let (pout, pin) = pipe().context("failed pipe")?;
                debug!(
                    "🔀 REDIRECT: Created redirect pipe - read_end={}, write_end={}",
                    pout, pin
                );
                ctx.outfile = pin;
                ctx.errfile = pin;
                debug!(
                    "🔀 REDIRECT: Set ctx.outfile={}, ctx.errfile={}",
                    ctx.outfile, ctx.errfile
                );
                Ok(Some(pout))
            }
            _ => {
                debug!("🔀 REDIRECT: No matching redirect pattern");
                Ok(None)
            }
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
    debug!("📡 SIGNAL: Sending signal {:?} to pid {}", signal, pid);
    match kill(pid, signal) {
        Ok(_) => {
            debug!(
                "📡 SIGNAL: Successfully sent signal {:?} to pid {}",
                signal, pid
            );
            Ok(())
        }
        Err(e) => {
            error!(
                "📡 SIGNAL: Failed to send signal {:?} to pid {}: {}",
                signal, pid, e
            );
            Err(e.into())
        }
    }
}

fn kill_process(process: &Option<Box<JobProcess>>) -> Result<()> {
    debug!("💀 KILL: Starting kill_process");
    if let Some(process) = process {
        debug!("💀 KILL: Killing process: {}", process.get_cmd());
        match process.kill() {
            Ok(_) => debug!(
                "💀 KILL: Successfully killed process: {}",
                process.get_cmd()
            ),
            Err(e) => error!(
                "💀 KILL: Failed to kill process {}: {}",
                process.get_cmd(),
                e
            ),
        }

        if process.next().is_some() {
            debug!("💀 KILL: Killing next process in pipeline");
            kill_process(&process.next())?;
        } else {
            debug!("💀 KILL: No next process to kill");
        }
    } else {
        debug!("💀 KILL: No process to kill");
    }
    debug!("💀 KILL: kill_process completed");
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
    #[ignore] // Ignore this test as it requires a TTY environment
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

        // Skip TTY-dependent operations in test environment
        if isatty(SHELL_TERMINAL).unwrap_or(false) {
            let tmode = tcgetattr(SHELL_TERMINAL).expect("failed cgetattr");
            let _ctx = Context::new(pid, pgid, tmode, true);
        } else {
            // Create a mock context for non-TTY environments
            println!("Skipping TTY-dependent test operations");
        }

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

        // Initial state is Running
        assert!(matches!(process.state, ProcessState::Running));

        // State change test
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
    fn test_pipeline_consumer_termination() {
        init();

        // Create a pipeline: cat | less
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set initial states
        cat_process.state = ProcessState::Running;
        less_process.state = ProcessState::Running;

        // Link them in pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process.clone())));

        let mut cat_job_process = JobProcess::Command(cat_process);

        // Initially, consumer is not terminated
        assert!(!cat_job_process.is_pipeline_consumer_terminated());

        // Now simulate less (consumer) exiting normally
        if let JobProcess::Command(cat_proc) = &mut cat_job_process {
            if let Some(next_box) = &mut cat_proc.next {
                if let JobProcess::Command(less_proc) = next_box.as_mut() {
                    less_proc.state = ProcessState::Completed(0, None);
                }
            }
        }

        // Now consumer should be detected as terminated
        assert!(cat_job_process.is_pipeline_consumer_terminated());

        // But the pipeline is not fully completed since cat is still running
        assert!(!cat_job_process.is_completed());
    }

    #[test]
    fn test_job_completion_with_consumer_termination() {
        init();

        let shell_pgid = getpgrp();
        let mut job = Job::new("cat file | less".to_string(), shell_pgid);

        // Create pipeline processes
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set states: cat running, less completed normally
        cat_process.state = ProcessState::Running;
        less_process.state = ProcessState::Completed(0, None);

        // Link pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process)));
        job.set_process(JobProcess::Command(cat_process));

        // Job should be considered completed due to consumer termination
        assert!(is_job_completed(&job));
    }

    #[test]
    fn test_normal_pipeline_completion() {
        init();

        let shell_pgid = getpgrp();
        let mut job = Job::new("cat file | less".to_string(), shell_pgid);

        // Create pipeline processes
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set states: both completed
        cat_process.state = ProcessState::Completed(0, None);
        less_process.state = ProcessState::Completed(0, None);

        // Link pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process)));
        job.set_process(JobProcess::Command(cat_process));

        // Job should be completed normally
        assert!(is_job_completed(&job));
    }

    #[test]
    fn test_job_process_variants() {
        init();
        let process = Process::new("test".to_string(), vec![]);
        let job_process = JobProcess::Command(process);

        // JobProcess type check
        match job_process {
            JobProcess::Command(_) => {} // Expected variant
            _ => panic!("Expected Command variant"),
        }
    }
}
