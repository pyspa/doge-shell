use super::job::Job;
use super::state::ProcessState;
use crate::process::wait::{is_job_completed, is_job_stopped};
use crate::shell::SHELL_TERMINAL;
use anyhow::{Context, Result};
use nix::sys::signal::Signal;
use nix::sys::signal::killpg;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{isatty, tcsetpgrp};
use std::os::fd::BorrowedFd;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error};

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

    if job.pty.is_none() {
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
        debug!("PTY job active, skipping tcsetpgrp (shell remains foreground to proxy I/O)");
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

    if job.pty.is_none() {
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
        debug!("PTY job active, skipping tcsetpgrp (shell remains foreground to proxy I/O)");
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

        job.set_process_state(pid, state);

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
            println!("\rdsh: job {} '{}' has stopped", job.job_id, job.cmd);
            break;
        }
    }
    Ok(())
}

pub async fn wait_process_no_hang(job: &mut Job) -> Result<()> {
    debug!("wait_process_no_hang started for job: {}", job.id);
    let mut send_killpg = false;
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
                check_background_all_output(job).await?;
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
            println!("\rdsh: job {} '{}' has stopped", job.job_id, job.cmd);
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

        let (pid, state) = match waitpid(None, Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG))
        {
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

        job.set_process_state(pid, state);

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
            println!("\rdsh: job {} '{}' has stopped", job.job_id, job.cmd);
            debug!("Job stopped, breaking from wait_process_no_hang_sync loop");
            break;
        }
    }
    debug!("wait_process_no_hang_sync completed for job: {}", job.id);
    Ok(())
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
        job.monitors[i].output_all(false).await?;
        i += 1;
    }
    debug!("check_background_all_output completed");
    Ok(())
}
