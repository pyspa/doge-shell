//! The `wait` builtin: wait for known async jobs and report their status.
//!
//! Operands are decimal PIDs only (`wait`, `wait PID`, `wait PID1 PID2
//! ...`). Job specs (`%1`) and `wait -n/-p/-f` are out of scope and
//! rejected: bare numbers are PIDs here, never job numbers. Statuses come
//! from `Job::final_exit_status()` using the frozen pipeline policy via the
//! completed-job finalizer; the ledger is consumed only here, never by
//! reconciliation (`jobs`, notices, `fg`/`bg`).

use crate::process::job_wait::{JobWaitOutcome, wait_for_termination};
use crate::shell::Shell;
use crate::shell::job::{FinalizeDrain, final_exit_status, finalize_completed_job};
use anyhow::Result;
use dsh_types::Context;
use nix::unistd::Pid;
use tracing::debug;

/// Outcome of waiting for a single PID.
enum WaitOneOutcome {
    /// The PID's status (a waited child, a retained completed status, or
    /// 127 for an unknown PID).
    Status(i32),
    /// `SIGINT` arrived mid-wait: the job was requeued untouched and the
    /// caller must report 130 without touching further operands.
    Interrupted,
}

/// `wait` entry point: bridge the async wait onto a runtime shared with
/// `fg`/`jobs`.
pub fn execute_wait(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<i32> {
    super::block_on_job_control_future(wait_async(shell, ctx, argv))?
}

/// Async body of `wait`.
async fn wait_async(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<i32> {
    let operands = argv.get(1..).unwrap_or(&[]);
    if operands.is_empty() {
        // Bare `wait`: every known async PID, status always 0 (individual
        // child failures do not become the builtin's status). All entries
        // are consumed.
        for pid in shell.known_async.known_pids() {
            match wait_one(shell, ctx, pid).await? {
                WaitOneOutcome::Status(_) => {}
                WaitOneOutcome::Interrupted => return Ok(130),
            }
        }
        return Ok(0);
    }
    let mut last_status = 0;
    for operand in operands {
        match wait_operand(shell, ctx, operand).await? {
            WaitOneOutcome::Status(status) => last_status = status,
            WaitOneOutcome::Interrupted => return Ok(130),
        }
    }
    Ok(last_status)
}

/// Wait for one operand: usage errors bail, unknown PIDs report 127 and let
/// the remaining operands run.
async fn wait_operand(shell: &mut Shell, ctx: &Context, operand: &str) -> Result<WaitOneOutcome> {
    if let Some(option) = operand.strip_prefix('-')
        && operand != "-"
    {
        // No pre-bail write: the builtin wrapper prints the `Err` once.
        anyhow::bail!("wait: unsupported option: -{option}");
    }
    let raw: i32 = match operand.parse() {
        Ok(raw) => raw,
        Err(_) => {
            let _ = ctx.write_stderr(&format!("wait: '{operand}': not a pid"));
            return Ok(WaitOneOutcome::Status(127));
        }
    };
    let pid = Pid::from_raw(raw);
    wait_one(shell, ctx, pid).await
}

/// Wait for one PID: an active table job is taken and terminated-waited; an
/// already-completed ledger entry needs no OS wait; anything else is
/// unknown (127) and never touches `waitpid`.
async fn wait_one(shell: &mut Shell, ctx: &Context, pid: Pid) -> Result<WaitOneOutcome> {
    if let Some(index) = shell.wait_jobs.iter().position(|job| job.pid == Some(pid)) {
        return wait_active_job(shell, index).await;
    }
    if let Some(exit_status) = shell.known_async.consume_completed(pid) {
        debug!("wait: pid {pid} already completed with status {exit_status}");
        return Ok(WaitOneOutcome::Status(exit_status));
    }
    if shell.known_async.remove(pid).is_some() {
        // Active ledger entry but no table job: stale ownership that can
        // never complete. Drop it rather than blocking forever.
        debug!("wait: pid {pid} has a stale active ledger entry, dropping it");
    }
    let _ = ctx.write_stderr(&format!("wait: '{pid}': not a child of this shell"));
    Ok(WaitOneOutcome::Status(127))
}

/// Termination-wait a table job under temporary ownership (`fg` model).
async fn wait_active_job(shell: &mut Shell, index: usize) -> Result<WaitOneOutcome> {
    let mut job = shell.wait_jobs.remove(index);
    match wait_for_termination(&mut job).await {
        Ok(JobWaitOutcome::Completed) => {
            let job = finalize_completed_job(shell, job, FinalizeDrain::ToEof).await?;
            let status = final_exit_status(&job).ok_or_else(|| {
                anyhow::anyhow!("wait: completed job {} has no final status", job.job_id)
            })?;
            // Archive first, then consume: the status reaches the caller
            // exactly once.
            let raw_pid = job.pid.map(|pid| pid.as_raw()).unwrap_or(-1);
            let consumed = job
                .pid
                .and_then(|pid| shell.known_async.consume_completed(pid));
            debug!(
                "wait: pid {raw_pid} completed with status {status} (ledger consumed: {})",
                consumed.is_some()
            );
            Ok(WaitOneOutcome::Status(status))
        }
        Ok(JobWaitOutcome::Stopped) => {
            // Termination waits never end stopped; reaching here means the
            // policy was bypassed. Requeue rather than orphan.
            debug!(
                "wait: unexpected stop outcome for job {}, requeuing",
                job.job_id
            );
            job.refresh_lifecycle_state();
            shell.wait_jobs.push(job);
            anyhow::bail!("wait: job stopped while waiting for termination")
        }
        Ok(JobWaitOutcome::Interrupted) => {
            job.refresh_lifecycle_state();
            shell.wait_jobs.push(job);
            debug!("wait: interrupted, job requeued as active");
            Ok(WaitOneOutcome::Interrupted)
        }
        Err(err) => {
            if job.is_process_tree_completed() {
                // Infrastructure error after completion: archive first so
                // the status survives, then report the error.
                if let Ok(finalized) =
                    finalize_completed_job(shell, job, FinalizeDrain::ToEof).await
                {
                    let _status = final_exit_status(&finalized);
                }
            } else {
                job.refresh_lifecycle_state();
                shell.wait_jobs.push(job);
            }
            Err(err)
        }
    }
}

impl dsh_builtin::shell_capabilities::JobControlCapability for Shell {
    fn wait_for_jobs(&mut self, ctx: &Context, argv: Vec<String>) -> Result<i32> {
        execute_wait(self, ctx, argv)
    }
}
