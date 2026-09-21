//! Background-job reconciliation and the canonical completed-job finalizer.
//!
//! `check_job_state` partitions the job table into completed and active,
//! retires every completed tree through `finalize_completed_job` (ReadyNow
//! monitor retirement, known-async ledger archive), streams active
//! background output, and re-polls stragglers through the same finalizer.
//! Completed `Job` values are dropped only on this path.

use crate::process::Job;
use crate::process::job_wait::{drain_completed_output, finalize_ready_output};
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

/// Terminate all background jobs with `SIGTERM`.
///
/// Shutdown escalation to `SIGKILL` lives in `kill_wait_jobs`
/// (`Shell::Drop`); there is no `SIGTERM` → grace period → `SIGKILL` state
/// machine here.
pub fn terminate_background_jobs(shell: &mut Shell) -> Result<()> {
    for job in &shell.wait_jobs {
        if !job.foreground {
            debug!("Terminating background job {}", job.job_id);
            let _ = job.signal(Signal::SIGTERM);
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

    // Indices of jobs that are completed and need to be removed (now we will collect completed jobs)

    // 1. First pass (Async): Update states for all jobs.
    // No output drain here: completed jobs drain exactly once inside
    // the canonical finalizer below, and active background jobs drain
    // in pass 3. Draining in both places doubled the per-monitor poll
    // budget and stalled reconciliation on descendant-held pipes.
    for (i, job) in shell.wait_jobs.iter_mut().enumerate() {
        debug!(
            "CHECK_JOB_STATE_CHECKING: Checking job {} (index: {}, pid: {:?}, state: {:?}, foreground: {})",
            job.job_id, i, job.pid, job.state, job.foreground
        );

        job.update_status();
    }

    // 2. Partition jobs into completed and active
    // We move all jobs out, partition them, and put active jobs back.
    // This avoids O(N^2) removal operations.
    let all_jobs = std::mem::take(&mut shell.wait_jobs);
    let (completed, active): (Vec<Job>, Vec<Job>) = all_jobs
        .into_iter()
        .partition(|job: &Job| job.is_process_tree_completed());

    shell.wait_jobs = active;

    // 3. Canonical finalization: every completed job retires its monitors
    // through `ReadyNow` (never waiting: a descendant may hold the pipe)
    // and archives its status in the known-async ledger before the heavy
    // `Job` is handed out (and dropped). No direct `remove()`.
    let mut completed_jobs = Vec::with_capacity(completed.len());
    for job in completed {
        completed_jobs.push(finalize_completed_job(shell, job, FinalizeDrain::ReadyNow).await?);
    }

    // 4. Active background jobs keep streaming their available output.
    for job in shell.wait_jobs.iter_mut() {
        if !job.foreground
            && let Err(e) = job.check_background_all_output().await
        {
            error!(
                "CHECK_JOB_STATE_BG_ERROR: Failed to check background output for job {}: {}",
                job.job_id, e
            );
        }
    }

    // 5. Re-poll: a straggler may have completed during the output poll
    // above. Newly completed jobs finalize through the same canonical path
    // with `ReadyNow`, which drains whatever the kernel holds without
    // waiting — including bytes written between the pass-4 poll and this
    // pass-5 completion observation.
    let active_jobs = std::mem::take(&mut shell.wait_jobs);
    for mut job in active_jobs {
        job.update_status();
        if job.is_process_tree_completed() {
            completed_jobs.push(finalize_completed_job(shell, job, FinalizeDrain::ReadyNow).await?);
        } else {
            shell.wait_jobs.push(job);
        }
    }

    // Logging for completed jobs
    for job in &completed_jobs {
        debug!(
            "CHECK_JOB_STATE_COMPLETED: Job {} completed (final state: {:?})",
            job.job_id, job.state
        );
    }

    let elapsed = start_time.elapsed();
    debug!(
        "CHECK_JOB_STATE_COMPLETE: Completed check in {}ms, {} jobs completed, {} jobs remaining",
        elapsed.as_millis(),
        completed_jobs.len(),
        shell.wait_jobs.len()
    );

    if elapsed.as_millis() > 10 {
        debug!(
            "CHECK_JOB_STATE_PERF: Job state check took {}ms (optimized)",
            elapsed.as_millis()
        );
    }

    Ok(completed_jobs)
}

/// Canonical final status of a completed job: the tail stage's
/// [`ProcessState::shell_exit_code`](crate::process::ProcessState::shell_exit_code).
///
/// Borrow-traverses the canonical tree (no clones) so normal exits,
/// `128+N` signals, pipeline tails, `NoCommand` stages, and builtin or
/// async-list helpers all report through one source of truth. Returns
/// `None` for a missing tree or a tail that has not completed.
///
/// A free function (not a `Job` method) so the 800-line `process/job.rs`
/// budget stays intact; the canonical consumer is the finalizer below.
pub(crate) fn final_exit_status(job: &Job) -> Option<i32> {
    let mut process = job.process.as_deref()?;
    while let Some(next) = process.next_process() {
        process = next;
    }
    process.get_state().shell_exit_code()
}

/// How a completed job's output monitors retire during finalization.
///
/// A completed `Job` may never be dropped before its monitors execute one
/// terminal retirement action: an explicit ownership wait drains to EOF,
/// and non-blocking reconciliation retires ready-now. No third option
/// exists: skipping the drain would drop bytes a completed child already
/// committed to the pipe.
pub(crate) enum FinalizeDrain {
    /// Block until every monitor reaches EOF. Explicit ownership waits
    /// (`wait PID`, `fg`) only: they own the wait by contract.
    ToEof,
    /// Retire without waiting. Reconciliation (`check_job_state`, hence
    /// `jobs`, notices, the background tick, `bg`) only.
    ///
    /// Sound because a fully-completed tree has closed every write end it
    /// owns: a direct non-blocking drain consumes everything still in the
    /// pipe buffer — including bytes written between an earlier poll and
    /// this completion observation — and only a descendant-held pipe can
    /// withhold the rest (its future output is out of scope, and no
    /// reconciliation may wait for its EOF without stalling the prompt).
    /// The pending fragment publishes at retirement with or without EOF,
    /// because monitor ownership ends here.
    ReadyNow,
}

/// Canonical completed-job finalizer: the only path that may drop a
/// completed `Job`.
///
/// Refreshes the lifecycle summary, requires a fully-completed canonical
/// tree (anything else bails loudly instead of dropping live ownership),
/// resolves the final status from the pipeline tail, drains every output
/// monitor according to `drain`, and archives the status in the
/// known-async ledger for later `wait PID` consumption. A drain error is
/// logged but never loses the status or the ownership: the ledger archive
/// still happens.
///
/// Reconciliation (`check_job_state`, `jobs`, notices, `fg`/`bg`, `wait`)
/// archives here but never consumes: only `wait PID` consumes the ledger.
pub(crate) async fn finalize_completed_job(
    shell: &mut Shell,
    mut job: Job,
    drain: FinalizeDrain,
) -> Result<Job> {
    job.refresh_lifecycle_state();
    if !job.is_process_tree_completed() {
        anyhow::bail!(
            "cannot finalize active job {} ('{}', state: {:?})",
            job.job_id,
            job.cmd,
            job.state,
        );
    }
    let status = final_exit_status(&job);
    let drain_result = match drain {
        FinalizeDrain::ToEof => drain_completed_output(&mut job).await,
        FinalizeDrain::ReadyNow => finalize_ready_output(&mut job),
    };
    if let Err(err) = drain_result {
        // The tree already completed: losing the retained status over a
        // monitor read error would be worse than losing the tail output.
        warn!(
            "finalize: output drain for completed job {} failed: {err:#}",
            job.job_id,
        );
    }
    if let Some(exit_status) = status {
        let pid = match job.pid {
            Some(pid) => Some(pid),
            None => shell
                .known_async
                .entry_by_job_id(job.job_id)
                .map(|entry| entry.pid),
        };
        if let Some(pid) = pid {
            shell.known_async.mark_completed(pid, exit_status);
        }
    }
    Ok(job)
}

pub fn kill_wait_jobs(shell: &mut Shell) -> Result<()> {
    let mut i = 0;
    while i < shell.wait_jobs.len() {
        shell.wait_jobs[i].kill()?;
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FinalizeDrain, finalize_completed_job};
    use crate::environment::Environment;
    use crate::process::io::OutputMonitor;
    use crate::process::{Job, JobProcess, Process, ProcessState};
    use crate::shell::Shell;
    use dsh_types::observed_output::ObservedStream;
    use std::io::Write as _;
    use std::time::Duration;

    fn test_shell() -> Shell {
        Shell::new(Environment::new())
    }

    /// A canonically-completed job carrying one monitor: no child is
    /// spawned (the tree state is already `Completed`), so the drain path
    /// itself is exercised deterministically.
    fn completed_job_with_monitor(pgid: nix::unistd::Pid, monitor: OutputMonitor) -> Job {
        let mut job = Job::new("test-completed &".to_string(), pgid);
        job.foreground = false;
        let mut process = Process::new(
            "test-completed".to_string(),
            vec!["test-completed".to_string()],
        );
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));
        job.refresh_lifecycle_state();
        job.monitors.push(monitor);
        assert!(
            job.is_process_tree_completed(),
            "test job must be canonically completed"
        );
        job
    }

    /// Bytes committed between an earlier poll and the completion
    /// observation survive reconciliation: the writer stays open (no EOF),
    /// so a `ToEof` drain would hang where `ReadyNow` returns.
    #[tokio::test]
    async fn finalize_ready_now_recovers_bytes_written_after_poll() {
        let mut shell = test_shell();
        let pgid = shell.pgid;
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let mut monitor = OutputMonitor::new(read, None, ObservedStream::Stdout).expect("monitor");
        let mut writer = std::fs::File::from(write);

        // Pass-4 style poll observes nothing.
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("poll");
        assert_eq!(monitor.captured_output, "");

        // Committed after the poll, before completion is observed.
        writer.write_all(b"FINAL-MARKER\n").expect("write marker");
        writer.flush().expect("flush");

        let job = completed_job_with_monitor(pgid, monitor);
        let finalized = finalize_completed_job(&mut shell, job, FinalizeDrain::ReadyNow)
            .await
            .expect("finalize");
        let captured: String = finalized
            .monitors
            .iter()
            .map(|monitor| monitor.captured_output.clone())
            .collect();
        assert!(
            captured.contains("FINAL-MARKER\n"),
            "bytes written after the poll were lost: {captured:?}"
        );
        drop(writer);
    }

    /// A no-newline fragment held across the running drain publishes at
    /// retirement even with the writer open and no EOF in sight.
    #[tokio::test]
    async fn finalize_ready_now_publishes_pending_fragment() {
        let mut shell = test_shell();
        let pgid = shell.pgid;
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let mut monitor = OutputMonitor::new(read, None, ObservedStream::Stdout).expect("monitor");
        let mut writer = std::fs::File::from(write);

        writer.write_all(b"NO-NEWLINE").expect("write fragment");
        writer.flush().expect("flush");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("running drain holds the fragment");
        assert_eq!(monitor.captured_output, "");

        let job = completed_job_with_monitor(pgid, monitor);
        let finalized = finalize_completed_job(&mut shell, job, FinalizeDrain::ReadyNow)
            .await
            .expect("finalize");
        let captured: String = finalized
            .monitors
            .iter()
            .map(|monitor| monitor.captured_output.clone())
            .collect();
        assert_eq!(captured, "NO-NEWLINE");
        drop(writer);
    }
}
