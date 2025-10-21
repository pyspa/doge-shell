use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, close, getpgrp, isatty, setpgid, tcsetpgrp};
use std::fs::File;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;
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
            let should_close = match input_fd {
                Some(fd) => stdin != fd,
                None => true,
            };
            if should_close {
                close(stdin).context("failed close")?;
            }
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

        if let Some(fd) = input_fd
            && ctx.infile == fd
        {
            ctx.infile = previous_infile;
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
                            time::sleep(Duration::from_millis(100)).await;
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
            let tmode = tcgetattr(SHELL_TERMINAL).expect("failed cgetattr");
            let _ctx = Context::new(pid, pgid, tmode, true);
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
