use crate::process::{Job, ProcessState};
use crate::shell::Shell;
use anyhow::Result;
use nix::sys::signal::Signal;
use tracing::{debug, error, warn};

pub fn get_next_job_id(shell: &mut Shell) -> usize {
    let id = shell.next_job_id;
    shell.next_job_id += 1;
    id
}

/// Send signal to foreground job
pub fn send_signal_to_foreground_job(shell: &mut Shell, signal: Signal) -> Result<()> {
    debug!(
        "SIGNAL_TO_FG_START: Attempting to send signal {:?} to foreground jobs (total jobs: {})",
        signal,
        shell.wait_jobs.len()
    );

    let mut sent_count = 0;
    let mut foreground_jobs = Vec::new();

    // First, collect information about foreground jobs
    for job in &shell.wait_jobs {
        if job.foreground {
            foreground_jobs.push((job.job_id, job.pid, job.cmd.clone()));
        }
    }

    debug!(
        "SIGNAL_TO_FG_TARGETS: Found {} foreground jobs to signal",
        foreground_jobs.len()
    );

    for (job_id, pid_opt, cmd) in &foreground_jobs {
        debug!(
            "SIGNAL_TO_FG_TARGET: Job {} (pid: {:?}, cmd: '{}')",
            job_id, pid_opt, cmd
        );
    }

    for job in &mut shell.wait_jobs {
        if job.foreground {
            if let Some(pid) = job.pid {
                debug!(
                    "SIGNAL_TO_FG_SENDING: Sending signal {:?} to foreground job {} (pid: {}, cmd: '{}')",
                    signal, job.job_id, pid, job.cmd
                );
                // Send signal to process group
                match nix::sys::signal::killpg(pid, signal) {
                    Ok(_) => {
                        debug!(
                            "SIGNAL_TO_FG_SUCCESS: Successfully sent signal {:?} to process group {} (job {})",
                            signal, pid, job.job_id
                        );
                        sent_count += 1;
                    }
                    Err(e) => {
                        warn!(
                            "SIGNAL_TO_FG_FALLBACK: Failed to send signal to process group {}: {}, trying individual process",
                            pid, e
                        );
                        // Fallback: send to individual process
                        match nix::sys::signal::kill(pid, signal) {
                            Ok(_) => {
                                debug!(
                                    "SIGNAL_TO_FG_FALLBACK_SUCCESS: Successfully sent signal {:?} to individual process {} (job {})",
                                    signal, pid, job.job_id
                                );
                                sent_count += 1;
                            }
                            Err(e2) => {
                                error!(
                                    "SIGNAL_TO_FG_FALLBACK_ERROR: Failed to send signal to individual process {}: {}",
                                    pid, e2
                                );
                            }
                        }
                    }
                }
            } else {
                warn!(
                    "SIGNAL_TO_FG_NO_PID: Foreground job {} has no PID, cannot send signal (cmd: '{}')",
                    job.job_id, job.cmd
                );
            }
            break;
        }
    }

    debug!(
        "SIGNAL_TO_FG_COMPLETE: Signal {:?} processing complete, {} signals sent out of {} foreground jobs",
        signal,
        sent_count,
        foreground_jobs.len()
    );

    if sent_count == 0 && !foreground_jobs.is_empty() {
        warn!(
            "SIGNAL_TO_FG_WARNING: No signals were sent despite having {} foreground jobs",
            foreground_jobs.len()
        );
    }

    Ok(())
}

/// Terminate all background jobs
pub fn terminate_background_jobs(shell: &mut Shell) -> Result<()> {
    for job in &mut shell.wait_jobs {
        if !job.foreground
            && let Some(pid) = job.pid
        {
            debug!("Terminating background job {} (pid: {})", job.job_id, pid);
            // Send SIGTERM first, then SIGKILL if needed
            let _ = nix::sys::signal::killpg(pid, Signal::SIGTERM);
        }
    }
    Ok(())
}

pub async fn check_job_state(shell: &mut Shell) -> Result<Vec<Job>> {
    // Fast path: no jobs to check
    if shell.wait_jobs.is_empty() {
        return Ok(Vec::new());
    }

    let start_time = std::time::Instant::now();

    debug!(
        "CHECK_JOB_STATE_START: Starting job state check, total jobs: {}",
        shell.wait_jobs.len()
    );

    // Log current job states before checking
    for (i, job) in shell.wait_jobs.iter().enumerate() {
        debug!(
            "CHECK_JOB_STATE_INITIAL: Job[{}] id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
            i, job.job_id, job.pid, job.state, job.foreground, job.cmd
        );
    }

    let mut completed: Vec<Job> = Vec::new();
    let mut i = 0;

    while i < shell.wait_jobs.len() {
        let job = &mut shell.wait_jobs[i];

        debug!(
            "CHECK_JOB_STATE_CHECKING: Checking job {} (index: {}, pid: {:?}, state: {:?}, foreground: {})",
            job.job_id, i, job.pid, job.state, job.foreground
        );

        // Single status evaluation; only check background output if not completed after first check
        let is_completed_now = job.update_status();
        if !is_completed_now && !job.foreground {
            debug!(
                "CHECK_JOB_STATE_BACKGROUND: Checking background output for job {}",
                job.job_id
            );
            if let Err(e) = job.check_background_all_output().await {
                error!(
                    "CHECK_JOB_STATE_BG_ERROR: Failed to check background output for job {}: {}",
                    job.job_id, e
                );
            }
            // Re-evaluate status only if background output was checked
            let is_completed_after_bg = job.update_status();
            if is_completed_after_bg {
                let removed_job = shell.wait_jobs.remove(i);
                debug!(
                    "CHECK_JOB_STATE_COMPLETED: Job {} completed and removed (final state: {:?}, exit_code: {})",
                    removed_job.job_id,
                    removed_job.state,
                    match removed_job.state {
                        ProcessState::Completed(code, _) => code.to_string(),
                        _ => "unknown".to_string(),
                    }
                );
                completed.push(removed_job);
                continue;
            }
        }

        if is_completed_now {
            let removed_job = shell.wait_jobs.remove(i);
            debug!(
                "CHECK_JOB_STATE_COMPLETED: Job {} completed and removed (final state: {:?}, exit_code: {})",
                removed_job.job_id,
                removed_job.state,
                match removed_job.state {
                    ProcessState::Completed(code, _) => code.to_string(),
                    _ => "unknown".to_string(),
                }
            );
            completed.push(removed_job);
        } else {
            debug!(
                "CHECK_JOB_STATE_ACTIVE: Job {} still active, continuing (state: {:?})",
                job.job_id, job.state
            );
            i += 1;
        }
    }

    let elapsed = start_time.elapsed();
    debug!(
        "CHECK_JOB_STATE_COMPLETE: Completed check in {}ms, {} jobs completed, {} jobs remaining",
        elapsed.as_millis(),
        completed.len(),
        shell.wait_jobs.len()
    );

    if elapsed.as_millis() > 10 {
        debug!(
            "CHECK_JOB_STATE_PERF: Job state check took {}ms for {} jobs (may indicate performance issue)",
            elapsed.as_millis(),
            shell.wait_jobs.len() + completed.len()
        );
    }

    Ok(completed)
}

pub fn kill_wait_jobs(shell: &mut Shell) -> Result<()> {
    let mut i = 0;
    while i < shell.wait_jobs.len() {
        shell.wait_jobs[i].kill()?;
        i += 1;
    }
    Ok(())
}
