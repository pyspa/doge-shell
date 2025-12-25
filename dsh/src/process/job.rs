use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::read;
use nix::unistd::{Pid, close, getpgrp, isatty, setpgid, tcsetpgrp};
use std::fs::File;
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::io::unix::AsyncFd;
use tokio::time;
use tracing::{debug, error};

use super::io::OutputMonitor;
use super::job_process::JobProcess;
use super::process::Process;
use super::redirect::Redirect;
use super::signal::send_signal;
use super::state::{ListOp, ProcessState, SubshellType};
use super::wait::{is_job_completed, is_job_stopped};
use crate::shell::{SHELL_TERMINAL, Shell};
use dsh_types::Context;

use crate::process::pty::Pty;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

#[derive(Debug, Clone, Copy)]
struct BorrowedFd(RawFd);

impl AsRawFd for BorrowedFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

struct NonBlockingFdGuard {
    fd: RawFd,
    original_flags: OFlag,
}

impl NonBlockingFdGuard {
    fn new(fd: RawFd) -> std::io::Result<Self> {
        let raw_flags = fcntl(fd, FcntlArg::F_GETFL).map_err(std::io::Error::other)?;
        let original_flags = OFlag::from_bits_truncate(raw_flags);
        let new_flags = original_flags | OFlag::O_NONBLOCK;
        if new_flags != original_flags {
            fcntl(fd, FcntlArg::F_SETFL(new_flags)).map_err(std::io::Error::other)?;
        }
        Ok(Self { fd, original_flags })
    }
}

impl Drop for NonBlockingFdGuard {
    fn drop(&mut self) {
        // Best-effort restore.
        let _ = fcntl(self.fd, FcntlArg::F_SETFL(self.original_flags));
    }
}

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
    monitors: Vec<OutputMonitor>,
    shell_pgid: Pid,
    /// Whether to capture output for $OUT variable
    pub capture_output: bool,
    pub pty: Option<Pty>,
    pub pty_output_task: Option<tokio::task::JoinHandle<Result<String>>>,
    pub pty_input_task: Option<tokio::task::JoinHandle<()>>,
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

        if ctx.foreground && ctx.interactive {
            // "Time Machine" logic: Use PTY for foreground jobs to capture output with color preservation.
            // Only enabled in interactive mode to avoid interference with test captures.
            match Pty::new() {
                Ok(pty) => {
                    debug!("PTY created: {:?}", pty);

                    // Resize PTY to match current terminal size
                    match crossterm::terminal::size() {
                        Ok((cols, rows)) => {
                            if let Err(e) = pty.resize(rows, cols) {
                                debug!("Failed to resize PTY: {}", e);
                            } else {
                                debug!("Resized PTY to {}x{}", cols, rows);
                            }
                        }
                        Err(e) => debug!("Failed to get terminal size for PTY resize: {}", e),
                    }

                    // Setup Output Monitor (Master -> Stdout + Capture)
                    // We need a clone of the master for reading
                    match pty.try_clone() {
                        Ok(pty_clone) => {
                            // Transfer ownership of master fd to monitor
                            let master_fd = pty_clone.master.into_raw_fd();
                            let mut monitor = super::io::PtyMonitor::new(master_fd);

                            let output_task = tokio::spawn(async move {
                                monitor.process_output().await?;
                                // Convert captured output to String (lossy) for history
                                // Stripping ANSI is done later (or we can do it here if we want to save space)
                                // For now save with ANSI.
                                Ok(String::from_utf8_lossy(&monitor.captured_output).to_string())
                            });
                            self.pty_output_task = Some(output_task);

                            // Setup Input Proxy (Stdin -> Master)
                            // This is needed because the job is running in a PTY,
                            // but we (Shell) are holding the Real Terminal.
                            // We must forward Real Stdin to PTY Master.
                            if ctx.interactive {
                                // Only setup input proxy if we are NOT a builtin
                                // Builtins usually run in-process and might not need PTY input,
                                // and holding tokio::io::stdin here can block/mess up the REPL.
                                let is_builtin = self
                                    .process
                                    .as_ref()
                                    .map(|p| matches!(**p, JobProcess::Builtin(_)))
                                    .unwrap_or(false);

                                if !is_builtin {
                                    match pty.try_clone() {
                                        Ok(pty_in) => {
                                            let mut master_write =
                                                tokio::fs::File::from_std(pty_in.master);
                                            let input_task = tokio::spawn(async move {
                                                let _nonblock =
                                                    NonBlockingFdGuard::new(STDIN_FILENO);
                                                let stdin = match AsyncFd::new(BorrowedFd(
                                                    STDIN_FILENO,
                                                )) {
                                                    Ok(fd) => fd,
                                                    Err(e) => {
                                                        error!(
                                                            "Failed to setup async stdin proxy for PTY: {}",
                                                            e
                                                        );
                                                        // Fallback to legacy stdin copy.
                                                        let mut stdin = tokio::io::stdin();
                                                        let _ = tokio::io::copy(
                                                            &mut stdin,
                                                            &mut master_write,
                                                        )
                                                        .await;
                                                        return;
                                                    }
                                                };

                                                let mut buf = [0u8; 4096];
                                                loop {
                                                    let mut readable = match stdin.readable().await
                                                    {
                                                        Ok(r) => r,
                                                        Err(e) => {
                                                            debug!(
                                                                "PTY stdin proxy: readable() failed: {}",
                                                                e
                                                            );
                                                            break;
                                                        }
                                                    };

                                                    let read_res =
                                                        readable.try_io(|_| {
                                                            match read(STDIN_FILENO, &mut buf) {
                                                            Ok(n) => Ok(n),
                                                            Err(Errno::EAGAIN) => Err(
                                                                std::io::Error::new(
                                                                    std::io::ErrorKind::WouldBlock,
                                                                    "stdin would block",
                                                                ),
                                                            ),
                                                            Err(e) => Err(std::io::Error::other(
                                                                e,
                                                            )),
                                                        }
                                                        });

                                                    let n = match read_res {
                                                        Ok(Ok(n)) => n,
                                                        Ok(Err(e)) => {
                                                            debug!(
                                                                "PTY stdin proxy: read failed: {}",
                                                                e
                                                            );
                                                            break;
                                                        }
                                                        Err(_would_block) => continue,
                                                    };

                                                    if n == 0 {
                                                        break;
                                                    }

                                                    if master_write
                                                        .write_all(&buf[..n])
                                                        .await
                                                        .is_err()
                                                    {
                                                        break;
                                                    }
                                                }
                                            });
                                            self.pty_input_task = Some(input_task);
                                        }
                                        Err(e) => error!("Failed to clone PTY for input: {}", e),
                                    }
                                }
                            }
                        }
                        Err(e) => error!("Failed to clone PTY for output: {}", e),
                    }

                    self.pty = Some(pty);
                }
                Err(e) => {
                    error!(
                        "Failed to create PTY: {}, falling back to normal execution",
                        e
                    );
                }
            }
        }

        let pty_slave_fd = self.pty.as_ref().map(|p| p.slave.as_raw_fd());

        if let Some(process) = self.process.take().as_mut() {
            debug!(
                "JOB_LAUNCH_PROCESS: Launching process for job {} (process_type: {})",
                self.job_id,
                process.get_cmd()
            );

            match self.launch_process(ctx, shell, process, pty_slave_fd) {
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
                    // Ensure cleanup of PTY tasks if launch fails
                    if let Some(input_task) = self.pty_input_task.take() {
                        input_task.abort();
                        let _ = input_task.await;
                    }
                    if let Some(output_task) = self.pty_output_task.take() {
                        output_task.abort();
                    }
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

                    // If PTY, we must enable raw mode to proxy input/output correctly
                    let raw_mode_enabled = if self.pty.is_some() {
                        match enable_raw_mode() {
                            Ok(_) => true,
                            Err(e) => {
                                error!("Failed to enable raw mode for PTY: {}", e);
                                false
                            }
                        }
                    } else {
                        false
                    };

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

                    if raw_mode_enabled {
                        // We should disable raw mode after put_in_foreground returns?
                        // put_in_foreground calls wait_job.
                        // So we want raw mode active DURING put_in_foreground.
                        let _ = disable_raw_mode();
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

        // Save output history if we captured anything
        // Consolidate captured output from monitors OR PTY task

        let mut stdout_cap = String::new();
        let mut stderr_cap = String::new();

        // Check PTY task first
        // process is completed, so we can drop the PTY structure.
        // potentially closing the slave FD held by us.
        // This is required so the master side sees EOF.
        self.pty = None;

        if let Some(output_task) = self.pty_output_task.take() {
            // We await the output task. It should finish when PTY master gets EOF (all slaves closed).
            // But if the process crashed without closing? Kernel handles it.
            // If we are here, process has Finished (wait_job returned).
            match output_task.await {
                Ok(Ok(output)) => {
                    // Stripping ANSI is handled here or by UI. User wanted preservation.
                    // We save raw output.
                    stdout_cap = output;
                }
                Ok(Err(e)) => error!("PTY output task failed: {}", e),
                Err(e) => error!("PTY output task join error: {}", e),
            }

            // Should also abort input task now if it's still running
            if let Some(input_task) = self.pty_input_task.take() {
                input_task.abort();
                // Ensure we wait for the task to finish cleanup (restore STDIN flags)
                // If we don't wait, the NonBlockingFdGuard destructor might run AFTER we return to the shell loop,
                // causing the shell to read STDIN in non-blocking mode and potentially miss input or error out.
                if let Err(e) = input_task.await
                    && !e.is_cancelled()
                {
                    error!("PTY input task join error: {}", e);
                }
            }
        } else {
            // Standard monitors (Pipe) logic
            // Assuming monitor[0] is stdout (based on launch_process order)
            let mut monitors_iter = self.monitors.iter();
            if let Some((Some(_), _)) = self.process.as_ref().map(|p| p.get_cap_out())
                && let Some(m) = monitors_iter.next()
            {
                stdout_cap = m.captured_output.clone();
            }
            if let Some((_, Some(_))) = self.process.as_ref().map(|p| p.get_cap_out())
                && let Some(m) = monitors_iter.next()
            {
                stderr_cap = m.captured_output.clone();
            }
        }

        if !stdout_cap.is_empty() || !stderr_cap.is_empty() {
            use dsh_types::output_history::OutputEntry;

            // Strip ANSI for history saving (User Rule: "color restoration is NOT needed for history")
            // Using console::strip_ansi_codes (we added dependency)
            let stdout_stripped = console::strip_ansi_codes(&stdout_cap).to_string();
            let stderr_stripped = console::strip_ansi_codes(&stderr_cap).to_string();

            if ctx.foreground {
                let exit_code = match self.state {
                    ProcessState::Completed(c, _) => c as i32,
                    _ => 0,
                };

                // Use stripped version for history
                let entry = OutputEntry::new(
                    self.cmd.clone(),
                    stdout_stripped,
                    stderr_stripped,
                    exit_code,
                );
                shell.environment.write().output_history.push(entry);
            }
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
        // For PTY jobs, we MUST NOT give the shell's terminal control to the job.
        // The shell proxies input/output to the PTY. If we give up the terminal,
        // the shell (backgrounded) cannot read from stdin to forward to the PTY.
        if self.pty.is_none() {
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
        } else {
            debug!("PTY job active, skipping tcsetpgrp (shell remains foreground to proxy I/O)");
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
        // For PTY jobs, we MUST NOT give the shell's terminal control to the job.
        // The shell proxies input/output to the PTY. If we give up the terminal,
        // the shell (backgrounded) cannot read from stdin to forward to the PTY.
        if self.pty.is_none() {
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
        } else {
            debug!("PTY job active, skipping tcsetpgrp (shell remains foreground to proxy I/O)");
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

        Ok(())
    }

    fn show_job_status(&self) {}

    pub async fn wait_job(&mut self, no_hang: bool) -> Result<()> {
        debug!("wait_job called with no_hang: {}", no_hang);
        // We ALWAYS use wait_process_no_hang (polling) to ensure output capturing is processed.
        // The blocking wait_process would deadlock pipes if output is large.
        debug!("Calling wait_process_no_hang (forced for output capture)");
        self.wait_process_no_hang().await
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

    fn wait_process_sync(&mut self) -> Result<()> {
        let mut send_killpg = false;
        loop {
            let (pid, state) = match waitpid(None, Some(WaitPidFlag::WUNTRACED)) {
                Ok(WaitStatus::Exited(pid, status)) => {
                    (pid, ProcessState::Completed(status as u8, None))
                }
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

            if is_job_completed(self) {
                debug!("⏳ WAIT: Job completed, breaking from wait_process loop");
                break;
            }

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
                            std::thread::sleep(Duration::from_millis(100));
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
            // Check for manual SIGINT forwarding
            if crate::process::signal::check_and_clear_sigint() {
                debug!("wait_process_no_hang: Detected SIGINT in parent shell, forwarding to job");
                if let Some(pgid) = self.pgid {
                    debug!("Forwarding SIGINT to pgid: {}", pgid);
                    let _ = killpg(pgid, Signal::SIGINT);
                } else if let Some(pid) = self.pid {
                    debug!("Forwarding SIGINT to pid: {}", pid);
                    let _ = nix::sys::signal::kill(pid, Signal::SIGINT);
                }
            }

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
                }
                Ok(Ok(WaitStatus::Signaled(pid, signal, _))) => {
                    debug!("wait_job signaled {:?} {:?}", pid, signal);
                    (pid, ProcessState::Completed(1, Some(signal)))
                }
                Ok(Ok(WaitStatus::Stopped(pid, signal))) => {
                    debug!("wait_job stopped {:?} {:?}", pid, signal);
                    (pid, ProcessState::Stopped(pid, signal))
                }
                Ok(Ok(WaitStatus::StillAlive)) => {
                    time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Ok(Err(nix::errno::Errno::ECHILD)) => {
                    self.check_background_all_output().await?;
                    break;
                }
                Ok(Err(nix::errno::Errno::EINTR)) => {
                    debug!("⏳ WAIT: waitpid interrupted by signal (EINTR), continuing");
                    continue;
                }
                status => {
                    error!("unexpected waitpid event: {:?}", status);
                    break;
                }
            };

            self.check_background_all_output().await?;
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
                debug!("Job completed, breaking from wait_process_no_hang loop");
                break;
            }

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
                            time::sleep(Duration::from_millis(100)).await;
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
            // Check for manual SIGINT forwarding
            if crate::process::signal::check_and_clear_sigint() {
                debug!(
                    "wait_process_no_hang_sync: Detected SIGINT in parent shell, forwarding to job"
                );
                if let Some(pgid) = self.pgid {
                    debug!("Forwarding SIGINT to pgid: {}", pgid);
                    let _ = killpg(pgid, Signal::SIGINT);
                } else if let Some(pid) = self.pid {
                    debug!("Forwarding SIGINT to pid: {}", pid);
                    let _ = nix::sys::signal::kill(pid, Signal::SIGINT);
                }
            }

            debug!("waitpid loop iteration...");

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
                    Err(nix::errno::Errno::EINTR) => {
                        debug!("⏳ WAIT: waitpid interrupted by signal (EINTR), continuing");
                        continue;
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
                            std::thread::sleep(Duration::from_millis(100));
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
    use nix::sys::termios::tcgetattr;
    use nix::unistd::{Pid, getpgrp, getpid};

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
            let tmode = match tcgetattr(SHELL_TERMINAL) {
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
