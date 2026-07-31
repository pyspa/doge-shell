use super::job::Job;
use super::job_process::JobProcess;
use super::state::ProcessState;
use crate::process::wait::{is_job_completed, is_job_stopped};
use crate::shell::SHELL_TERMINAL;
use anyhow::{Context, Result};
use nix::sys::signal::Signal;
use nix::sys::signal::killpg;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, getpid, isatty, tcsetpgrp};
use std::os::fd::BorrowedFd;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error};

/// Report a job that just stopped, in the same `[1]+  Stopped  cmd` format bash
/// uses. Without it Ctrl-Z looks like it did nothing.
fn print_stopped_notice(job: &Job) {
    use crate::repl::job_notify::{JobMarker, JobNotice, JobNoticeState, format_job_notice};
    use std::io::Write;

    let notice = format_job_notice(&JobNotice {
        job_id: job.job_id,
        cmd: job.cmd.clone(),
        state: JobNoticeState::Stopped,
        marker: JobMarker::Current,
    });
    // Leading \r: in raw mode the cursor is mid-line when the job stops.
    println!("\r{}", notice);
    let _ = std::io::stdout().flush();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnownWaitResult {
    State(Pid, ProcessState),
    StillAlive,
    NoChildren,
}

/// Poll delay bounds for the `WNOHANG` wait loops.
///
/// The first `waitpid` of a loop runs immediately after `fork`, so it practically
/// always reports `StillAlive`. With a single fixed delay every command — even one
/// that exits in a millisecond — paid that delay before the shell noticed and
/// redrew the prompt. Starting small and backing off keeps short commands snappy
/// while long-running jobs settle on a cheap poll rate.
const WAIT_POLL_MIN: Duration = Duration::from_millis(1);
const WAIT_POLL_MAX: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy)]
struct WaitBackoff {
    delay: Duration,
}

impl WaitBackoff {
    fn new() -> Self {
        Self {
            delay: WAIT_POLL_MIN,
        }
    }

    /// Delay to sleep before the next `waitpid`, doubling up to [`WAIT_POLL_MAX`].
    fn next_delay(&mut self) -> Duration {
        let delay = self.delay;
        self.delay = (self.delay * 2).min(WAIT_POLL_MAX);
        delay
    }

    /// A process just changed state; its siblings are likely to follow shortly,
    /// so go back to polling tightly.
    fn reset(&mut self) {
        self.delay = WAIT_POLL_MIN;
    }
}

pub async fn put_in_foreground(job: &mut Job, no_hang: bool, cont: bool) -> Result<()> {
    debug!(
        "put_in_foreground: id: {} pgid {:?} no_hang: {} cont: {}",
        job.id, job.pgid, no_hang, cont
    );

    if !isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false) {
        debug!("Not a terminal environment, skipping process group control");
        debug!("About to call wait_job with no_hang: {}", no_hang);
        wait_job(job, no_hang).await?;
        debug!("wait_job completed in non-terminal mode");
        return Ok(());
    }

    debug!("Terminal environment detected, proceeding with process group control");

    if !crate::process::job_pty::uses_full_pty_proxy(job) {
        if let Some(pgid) = job.pgid {
            debug!("Setting foreground process group to {}", pgid);
            if let Err(err) = tcsetpgrp(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }, pgid) {
                debug!(
                    "tcsetpgrp failed: {}, continuing without terminal control",
                    err
                );
            } else {
                debug!("Successfully set foreground process group to {}", pgid);
            }

            if cont {
                debug!("Sending SIGCONT to process group {}", pgid);
                crate::process::signal::send_signal(pgid, Signal::SIGCONT)
                    .context("failed send signal SIGCONT")?;
                debug!("SIGCONT sent successfully");
            }
        } else {
            debug!("No pgid available, skipping process group operations");
        }
    } else {
        debug!("Full-proxy PTY job active, skipping tcsetpgrp (shell proxies I/O)");
    }

    debug!("About to call wait_job with no_hang: {}", no_hang);
    wait_job(job, no_hang).await?;
    debug!("wait_job completed");

    let shell_pgid = job.shell_pgid;
    debug!("Restoring shell process group {}", shell_pgid);
    if let Err(err) = tcsetpgrp(
        unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) },
        shell_pgid,
    ) {
        debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
    } else {
        debug!("Successfully restored shell process group {}", shell_pgid);
    }

    debug!("put_in_foreground completed successfully");
    Ok(())
}

pub fn put_in_foreground_sync(job: &mut Job, no_hang: bool, cont: bool) -> Result<()> {
    debug!(
        "put_in_foreground_sync: id: {} pgid {:?} no_hang: {} cont: {}",
        job.id, job.pgid, no_hang, cont
    );

    if !isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false) {
        debug!("Not a terminal environment, skipping process group control");
        debug!("About to call wait_job_sync with no_hang: {}", no_hang);
        wait_job_sync(job, no_hang)?;
        debug!("wait_job_sync completed in non-terminal mode");
        return Ok(());
    }

    debug!("Terminal environment detected, proceeding with process group control");

    if !crate::process::job_pty::uses_full_pty_proxy(job) {
        if let Some(pgid) = job.pgid {
            debug!("Setting foreground process group to {}", pgid);
            if let Err(err) = tcsetpgrp(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }, pgid) {
                debug!(
                    "tcsetpgrp failed: {}, continuing without terminal control",
                    err
                );
            } else {
                debug!("Successfully set foreground process group to {}", pgid);
            }

            if cont {
                debug!("Sending SIGCONT to process group {}", pgid);
                crate::process::signal::send_signal(pgid, Signal::SIGCONT)
                    .context("failed send signal SIGCONT")?;
                debug!("SIGCONT sent successfully");
            }
        } else {
            debug!("No pgid available, skipping process group operations");
        }
    } else {
        debug!("Full-proxy PTY job active, skipping tcsetpgrp (shell proxies I/O)");
    }

    debug!("About to call wait_job_sync with no_hang: {}", no_hang);
    wait_job_sync(job, no_hang)?;
    debug!("wait_job_sync completed");

    let shell_pgid = job.shell_pgid;
    debug!("Restoring shell process group {}", shell_pgid);
    if let Err(err) = tcsetpgrp(
        unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) },
        shell_pgid,
    ) {
        debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
    } else {
        debug!("Successfully restored shell process group {}", shell_pgid);
    }

    debug!("put_in_foreground_sync completed successfully");
    Ok(())
}

pub async fn put_in_background(job: &mut Job) -> Result<()> {
    debug!("put_in_background pgid {:?}", job.pgid,);

    if !isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false) {
        debug!("Not a terminal environment, skipping process group control");
        return Ok(());
    }

    if let Err(err) = tcsetpgrp(
        unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) },
        job.shell_pgid,
    ) {
        debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
    } else {
        debug!(
            "Successfully set background process group to shell {}",
            job.shell_pgid
        );
    }
    Ok(())
}

pub async fn wait_job(job: &mut Job, no_hang: bool) -> Result<()> {
    debug!("wait_job called with no_hang: {}", no_hang);
    debug!("Calling wait_process_no_hang (forced for output capture)");
    wait_process_no_hang(job).await
}

pub fn wait_job_sync(job: &mut Job, no_hang: bool) -> Result<()> {
    debug!("wait_job_sync called with no_hang: {}", no_hang);
    if no_hang {
        debug!("Calling wait_process_no_hang_sync");
        wait_process_no_hang_sync(job)
    } else {
        debug!("Calling wait_process (blocking)");
        wait_process_sync(job)
    }
}

pub fn wait_process_sync(job: &mut Job) -> Result<()> {
    let mut send_killpg = false;
    let mut backoff = WaitBackoff::new();
    loop {
        let (pid, state) = match wait_known_processes(job, WaitPidFlag::WUNTRACED) {
            Ok(KnownWaitResult::State(pid, state)) => (pid, state),
            Ok(KnownWaitResult::StillAlive) => {
                std::thread::sleep(backoff.next_delay());
                continue;
            }
            Ok(KnownWaitResult::NoChildren) | Err(nix::errno::Errno::ECHILD) => {
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

        job.set_process_state(pid, state);
        backoff.reset();

        debug!(
            "fin waitpid pgid:{:?} pid:{:?} state:{:?}",
            job.pgid, pid, state
        );

        if let ProcessState::Completed(code, signal) = state {
            debug!(
                "⏳ WAIT: Process completed - pid: {}, code: {}, signal: {:?}",
                pid, code, signal
            );
            if code != 0 && !send_killpg {
                if let Some(pgid) = job.pgid {
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

        if is_job_completed(job) {
            debug!("⏳ WAIT: Job completed, breaking from wait_process loop");
            break;
        }

        if let Some(process) = &job.process
            && process.is_pipeline_consumer_terminated()
            && !process.is_completed()
        {
            debug!("⏳ WAIT: Pipeline consumer terminated, killing remaining processes");
            if let Some(pgid) = job.pgid {
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

        if is_job_stopped(job) {
            debug!("⏳ WAIT: Job stopped");
            print_stopped_notice(job);
            break;
        }
    }
    Ok(())
}

pub async fn wait_process_no_hang(job: &mut Job) -> Result<()> {
    debug!("wait_process_no_hang started for job: {}", job.id);
    let mut send_killpg = false;
    let mut backoff = WaitBackoff::new();
    loop {
        if crate::process::signal::check_and_clear_sigint() {
            debug!("wait_process_no_hang: Detected SIGINT in parent shell, forwarding to job");
            if let Some(pgid) = job.pgid {
                debug!("Forwarding SIGINT to pgid: {}", pgid);
                let _ = killpg(pgid, Signal::SIGINT);
            } else if let Some(pid) = job.pid {
                debug!("Forwarding SIGINT to pid: {}", pid);
                let _ = nix::sys::signal::kill(pid, Signal::SIGINT);
            }
        }

        debug!("waitpid loop iteration...");

        check_background_all_output(job).await?;

        let wait_pids = job_wait_pids(job);
        let (pid, state) = match tokio::task::spawn_blocking(move || {
            wait_known_pids(&wait_pids, WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG)
        })
        .await
        {
            Ok(Ok(KnownWaitResult::State(pid, state))) => (pid, state),
            Ok(Ok(KnownWaitResult::StillAlive)) => {
                time::sleep(backoff.next_delay()).await;
                continue;
            }
            Ok(Ok(KnownWaitResult::NoChildren)) | Ok(Err(nix::errno::Errno::ECHILD)) => {
                check_background_all_output(job).await?;
                drain_foreground_completed_output(job).await?;
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

        check_background_all_output(job).await?;
        job.set_process_state(pid, state);
        backoff.reset();

        debug!("fin wait: pid:{:?}", pid);

        if let ProcessState::Completed(code, _) = state
            && code != 0
            && !send_killpg
            && let Some(pgid) = job.pgid
        {
            debug!("killpg pgid: {}", pgid);
            let _ = killpg(pgid, Signal::SIGKILL);
            send_killpg = true;
        }

        if is_job_completed(job) {
            debug!("Job completed, breaking from wait_process_no_hang loop");
            drain_foreground_completed_output(job).await?;
            break;
        }

        if let Some(process) = &job.process
            && process.is_pipeline_consumer_terminated()
            && !process.is_completed()
        {
            debug!("Pipeline consumer terminated, killing remaining processes");
            if let Some(pgid) = job.pgid {
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

        if is_job_stopped(job) {
            print_stopped_notice(job);
            debug!("Job stopped, breaking from wait_process_no_hang loop");
            break;
        }
    }
    debug!("wait_process_no_hang completed for job: {}", job.id);
    Ok(())
}

pub fn wait_process_no_hang_sync(job: &mut Job) -> Result<()> {
    debug!("wait_process_no_hang_sync started for job: {}", job.id);
    let mut send_killpg = false;
    let mut backoff = WaitBackoff::new();
    loop {
        if crate::process::signal::check_and_clear_sigint() {
            debug!("wait_process_no_hang_sync: Detected SIGINT in parent shell, forwarding to job");
            if let Some(pgid) = job.pgid {
                debug!("Forwarding SIGINT to pgid: {}", pgid);
                let _ = killpg(pgid, Signal::SIGINT);
            } else if let Some(pid) = job.pid {
                debug!("Forwarding SIGINT to pid: {}", pid);
                let _ = nix::sys::signal::kill(pid, Signal::SIGINT);
            }
        }

        debug!("waitpid loop iteration...");

        let (pid, state) =
            match wait_known_processes(job, WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG) {
                Ok(KnownWaitResult::State(pid, state)) => (pid, state),
                Ok(KnownWaitResult::StillAlive) => {
                    std::thread::sleep(backoff.next_delay());
                    continue;
                }
                Ok(KnownWaitResult::NoChildren) | Err(nix::errno::Errno::ECHILD) => {
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

        job.set_process_state(pid, state);
        backoff.reset();

        debug!("fin wait: pid:{:?}", pid);

        if let ProcessState::Completed(code, _) = state
            && code != 0
            && !send_killpg
            && let Some(pgid) = job.pgid
        {
            debug!("killpg pgid: {}", pgid);
            let _ = killpg(pgid, Signal::SIGKILL);
            send_killpg = true;
        }

        if is_job_completed(job) {
            debug!("Job completed, breaking from wait_process_no_hang_sync loop");
            break;
        }

        if let Some(process) = &job.process
            && process.is_pipeline_consumer_terminated()
            && !process.is_completed()
        {
            debug!("Pipeline consumer terminated, killing remaining processes");
            if let Some(pgid) = job.pgid {
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

        if is_job_stopped(job) {
            print_stopped_notice(job);
            debug!("Job stopped, breaking from wait_process_no_hang_sync loop");
            break;
        }
    }
    debug!("wait_process_no_hang_sync completed for job: {}", job.id);
    Ok(())
}

fn wait_known_processes(job: &Job, flags: WaitPidFlag) -> nix::Result<KnownWaitResult> {
    let wait_pids = job_wait_pids(job);
    wait_known_pids(&wait_pids, flags)
}

fn wait_known_pids(pids: &[Pid], flags: WaitPidFlag) -> nix::Result<KnownWaitResult> {
    if pids.is_empty() {
        return Ok(KnownWaitResult::NoChildren);
    }

    let mut saw_alive = false;
    let mut saw_child = false;
    for pid in pids {
        match waitpid(*pid, Some(flags | WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, status)) => {
                debug!("wait_job exited {:?} {:?}", pid, status);
                return Ok(KnownWaitResult::State(
                    pid,
                    ProcessState::Completed(status as u8, None),
                ));
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) => {
                debug!("wait_job signaled {:?} {:?}", pid, signal);
                return Ok(KnownWaitResult::State(
                    pid,
                    ProcessState::Completed(1, Some(signal)),
                ));
            }
            Ok(WaitStatus::Stopped(pid, signal)) => {
                debug!("wait_job stopped {:?} {:?}", pid, signal);
                return Ok(KnownWaitResult::State(
                    pid,
                    ProcessState::Stopped(pid, signal),
                ));
            }
            Ok(WaitStatus::StillAlive) => {
                saw_child = true;
                saw_alive = true;
            }
            Err(nix::errno::Errno::ECHILD) => {}
            Err(nix::errno::Errno::EINTR) => return Err(nix::errno::Errno::EINTR),
            status => {
                error!(
                    "unexpected waitpid event for known pid {}: {:?}",
                    pid, status
                );
            }
        }
    }

    if saw_alive || saw_child {
        Ok(KnownWaitResult::StillAlive)
    } else {
        Ok(KnownWaitResult::NoChildren)
    }
}

fn job_wait_pids(job: &Job) -> Vec<Pid> {
    let current_pid = getpid();
    let mut pids = Vec::new();
    if let Some(process) = &job.process {
        collect_process_wait_pids(process, current_pid, &mut pids);
    }
    if pids.is_empty()
        && let Some(pid) = job.pid
        && pid != current_pid
    {
        pids.push(pid);
    }
    pids.sort_unstable_by_key(|pid| pid.as_raw());
    pids.dedup();
    pids
}

fn collect_process_wait_pids(process: &JobProcess, current_pid: Pid, pids: &mut Vec<Pid>) {
    match process {
        JobProcess::Builtin(process) => {
            if let Some(pid) = process.pid
                && pid != current_pid
            {
                pids.push(pid);
            }
            if let Some(next) = &process.next {
                collect_process_wait_pids(next, current_pid, pids);
            }
        }
        JobProcess::Command(process) => {
            if let Some(pid) = process.pid {
                pids.push(pid);
            }
            if let Some(next) = &process.next {
                collect_process_wait_pids(next, current_pid, pids);
            }
        }
    }
}

pub async fn check_background_output(job: &mut Job) -> Result<()> {
    let mut i = 0;
    while i < job.monitors.len() {
        let _ = job.monitors[i].output().await?;
        i += 1;
    }
    Ok(())
}

pub async fn check_background_all_output(job: &mut Job) -> Result<()> {
    debug!(
        "check_background_all_output: monitors.len() = {}",
        job.monitors.len()
    );
    let mut i = 0;
    while i < job.monitors.len() {
        debug!("Processing monitor {}", i);
        job.monitors[i].drain_available().await?;
        i += 1;
    }
    debug!("check_background_all_output completed");
    Ok(())
}

pub async fn drain_foreground_completed_output(job: &mut Job) -> Result<()> {
    debug!(
        "drain_foreground_completed_output: monitors.len() = {}",
        job.monitors.len()
    );
    let mut i = 0;
    while i < job.monitors.len() {
        debug!("Draining completed monitor {}", i);
        job.monitors[i].drain_to_eof().await?;
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::process::Process;
    use nix::unistd::getpgrp;
    use std::process::{Command as StdCommand, Stdio};

    #[test]
    fn job_wait_does_not_reap_unrelated_completion_child() {
        let unrelated = StdCommand::new("sh")
            .arg("-c")
            .arg("printf unrelated")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn unrelated child");
        let mut job_child = StdCommand::new("sh")
            .arg("-c")
            .arg("sleep 0.05")
            .spawn()
            .expect("spawn job child");

        let mut job = Job::new("test".to_string(), getpgrp());
        let job_pid = Pid::from_raw(job_child.id() as i32);
        let mut process = Process::new("sh".to_string(), vec![]);
        process.pid = Some(job_pid);
        job.pid = Some(job_pid);
        job.set_process(JobProcess::Command(process));

        wait_process_no_hang_sync(&mut job).expect("wait job");

        let output = unrelated.wait_with_output().expect("wait unrelated child");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unrelated");
        let _ = job_child.wait();
    }
}
