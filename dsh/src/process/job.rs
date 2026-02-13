use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::{Pid, close, getpgrp, setpgid};
use std::fs::File;
use std::os::unix::io::{AsRawFd, RawFd};
use tracing::{debug, error};

use super::io::OutputMonitor;
use super::job_process::JobProcess;
use super::process::Process;
use super::redirect::Redirect;
use super::state::{ListOp, ProcessState, SubshellType};
use super::wait::is_job_completed;
use crate::process::pty::Pty;
use crate::shell::Shell;
use dsh_types::Context;

use crate::process::job_pty;
use crate::process::job_wait;

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub cmd: String,
    pub pid: Option<Pid>,
    pub pgid: Option<Pid>,
    pub(crate) process: Option<Box<JobProcess>>,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    pub foreground: bool,
    pub subshell: SubshellType,
    pub redirect: Option<Redirect>,
    pub list_op: ListOp,
    pub job_id: usize,
    pub state: ProcessState,
    pub(crate) monitors: Vec<OutputMonitor>,
    pub(crate) shell_pgid: Pid,
    /// Whether to capture output for $OUT variable
    pub capture_output: bool,
    pub pty: Option<Pty>,
    pub pty_output_task: Option<tokio::task::JoinHandle<Result<String>>>,
    pub pty_input_task: Option<tokio::task::JoinHandle<()>>,
    pub disable_pty: bool,
    /// Lisp expressions to evaluate after command output (from |: operator)
    pub struct_pipe_exprs: Vec<String>,
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
            capture_output: false,
            pty: None,
            pty_output_task: None,
            pty_input_task: None,
            disable_pty: false,
            struct_pipe_exprs: Vec::new(),
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
            capture_output: false,
            pty: None,
            pty_output_task: None,
            pty_input_task: None,
            disable_pty: false,
            struct_pipe_exprs: Vec::new(),
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

        // 1. Setup PTY if needed
        let pty_slave_fd = self.setup_pty(ctx).await?;

        // 2. Launch processes
        if let Some(mut process) = self.process.take() {
            debug!(
                "JOB_LAUNCH_PROCESS: Launching process for job {} (process_type: {})",
                self.job_id,
                process.get_cmd()
            );

            if let Err(e) = self.launch_process(ctx, shell, &mut process, pty_slave_fd) {
                error!(
                    "JOB_LAUNCH_PROCESS_ERROR: Failed to launch process for job {}: {}",
                    self.job_id, e
                );
                self.cleanup_pty_tasks().await;
                return Err(e);
            }

            // 3. Manage execution (Foreground/Background)
            self.manage_execution(ctx).await?;
        } else {
            debug!(
                "JOB_LAUNCH_NO_PROCESS: Job {} has no process to launch",
                self.job_id
            );
        }

        // 4. Capture output and save to history
        self.capture_output_and_history(ctx, shell).await?;

        let final_state = if ctx.foreground {
            self.last_process_state()
        } else {
            ProcessState::Running
        };

        debug!(
            "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
            self.job_id, final_state, ctx.foreground
        );

        Ok(final_state)
    }

    pub(crate) async fn setup_pty(&mut self, ctx: &mut Context) -> Result<Option<RawFd>> {
        job_pty::setup_pty(self, ctx).await
    }

    #[allow(dead_code)]
    pub(crate) async fn setup_pty_input_proxy(&mut self, pty_in: Pty) {
        job_pty::setup_pty_input_proxy(self, pty_in).await
    }

    pub(crate) async fn cleanup_pty_tasks(&mut self) {
        job_pty::cleanup_pty_tasks(self).await
    }

    async fn manage_execution(&mut self, ctx: &mut Context) -> Result<()> {
        job_pty::manage_execution(self, ctx).await
    }

    async fn capture_output_and_history(&mut self, ctx: &Context, shell: &mut Shell) -> Result<()> {
        job_pty::capture_output_and_history(self, ctx, shell).await
    }

    fn launch_process(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        process: &mut JobProcess,
        pty_slave: Option<RawFd>,
    ) -> Result<()> {
        let previous_infile = ctx.infile;
        let mut _input_file_guard: Option<File> = None;
        let mut input_fd: Option<RawFd> = None;

        if let Some(Redirect::Input(ref path)) = self.redirect {
            let file = File::open(path)
                .with_context(|| format!("failed to open input redirect file '{}'", path))?;
            let fd = file.as_raw_fd();
            ctx.infile = fd;
            input_fd = Some(fd);
            _input_file_guard = Some(file);
        }

        // Use launch for automatic capture (modified internal logic)
        let (pid, mut next_process) =
            process.launch(ctx, shell, &self.redirect, self.stdout, pty_slave)?;
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

            // For PTY jobs, we skip setting the process group in the parent.
            // The child will call setsid() to create a new session and become the leader.
            // Calling setpgid() here would make the child a process group leader in the shell's session,
            // which causes setsid() in the child to fail with EPERM.
            if pty_slave.is_none() {
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
            } else {
                debug!(
                    "Skipping parent setpgid for PTY job (child {} will setsid)",
                    pid
                );
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
            let should_close = match input_fd {
                Some(fd) => stdin != fd,
                None => pty_slave != Some(stdin), // Don't close if it's pty_slave
            };
            if should_close && let Err(e) = close(stdin) {
                debug!("failed close stdin: {}", e);
                // Don't error out here, just log (avoid crash if EBADF)
            }
        }
        if stdout != self.stdout
            && pty_slave != Some(stdout)
            && let Err(e) = close(stdout)
        {
            debug!("failed close stdout: {}", e);
        }
        if stderr != self.stderr
            && stdout != stderr
            && pty_slave != Some(stderr)
            && let Err(e) = close(stderr)
        {
            debug!("failed close stderr: {}", e);
        }

        self.set_process(process.to_owned());
        self.show_job_status();

        if let Some(ref redirect) = self.redirect {
            redirect.process(ctx);
        }

        if let Some(fd) = input_fd
            && ctx.infile == fd
        {
            ctx.infile = previous_infile;
        }

        // run next pipeline process
        if let Some(Err(err)) = next_process
            .take()
            .as_mut()
            .map(|process| self.launch_process(ctx, shell, process, pty_slave))
        {
            debug!("err {:?}", err);
            return Err(err);
        }

        Ok(())
    }

    pub async fn put_in_foreground(&mut self, no_hang: bool, cont: bool) -> Result<()> {
        job_wait::put_in_foreground(self, no_hang, cont).await
    }

    /// Synchronous version of put_in_foreground for use in non-async contexts
    /// This method uses spawn_blocking to handle the async operations safely
    pub fn put_in_foreground_sync(&mut self, no_hang: bool, cont: bool) -> Result<()> {
        job_wait::put_in_foreground_sync(self, no_hang, cont)
    }

    pub async fn put_in_background(&mut self) -> Result<()> {
        job_wait::put_in_background(self).await
    }

    fn show_job_status(&self) {}

    pub async fn wait_job(&mut self, no_hang: bool) -> Result<()> {
        job_wait::wait_job(self, no_hang).await
    }

    /// Synchronous version of wait_job for use in non-async contexts
    pub fn wait_job_sync(&mut self, no_hang: bool) -> Result<()> {
        job_wait::wait_job_sync(self, no_hang)
    }

    pub(crate) fn set_process_state(&mut self, pid: Pid, state: ProcessState) {
        if let Some(process) = self.process.as_mut() {
            process.set_state_pid(pid, state);
        }
    }

    #[allow(dead_code)]
    pub async fn check_background_output(&mut self) -> Result<()> {
        job_wait::check_background_output(self).await
    }

    pub async fn check_background_all_output(&mut self) -> Result<()> {
        job_wait::check_background_all_output(self).await
    }

    pub fn kill(&mut self) -> Result<()> {
        use super::signal::kill_process;
        kill_process(&self.process)
    }

    pub fn update_status(&mut self) -> bool {
        let old_state = self.state;

        if let Some(process) = self.process.as_mut()
            && let Some(state) = process.update_state()
        {
            self.state = state;

            if old_state != self.state {
                debug!(
                    "JOB_STATE_CHANGE: Job {} state changed: {:?} -> {:?} (pid: {:?}, pgid: {:?})",
                    self.job_id, old_state, self.state, self.pid, self.pgid
                );

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::wait::is_job_completed;
    use crate::shell::SHELL_TERMINAL;
    use nix::sys::termios::tcgetattr;
    use nix::unistd::{Pid, getpgrp, getpid, isatty};
    use std::os::fd::BorrowedFd;

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
        if isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false) {
            let tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }) {
                Ok(mode) => mode,
                Err(_) => return Ok(()),
            };
            let _ctx = Context::new(pid, pgid, Some(tmode), true);
        } else {
            // Create a mock context for non-TTY environments
            println!("Skipping TTY-dependent test operations");
        }

        Ok(())
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
}
