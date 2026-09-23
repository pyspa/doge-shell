//! `fg` resume driver and foreground/background reconciliation.
//!
//! `execute_fg` is the thin sync bridge over [`foreground_selected_job`];
//! the `finalize_*` reconcilers return still-active jobs to `wait_jobs` and
//! drop completed trees only through the canonical completed-job finalizer.

use super::parse_job_spec;
use crate::process::ProcessState;
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use tracing::{debug, error};

/// Execute the `fg` builtin command.
///
/// Brings a background job to the foreground.
///
/// Thin sync bridge over [`foreground_selected_job`]: the async driver owns
/// the wait (and its `OutputMonitor` drain), while this boundary only blocks
/// the calling worker thread until it completes.
pub fn execute_fg(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    debug!(
        "FG_CMD_START: Starting fg command - wait_jobs.len(): {}, args: {:?}",
        shell.wait_jobs.len(),
        argv
    );

    if shell.wait_jobs.is_empty() {
        debug!("FG_CMD_NO_JOBS: No jobs available for fg command");
        ctx.write_stdout("fg: there are no suitable jobs")?;
        return Ok(());
    }

    let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
    debug!("FG_CMD_SPEC: Job specification: '{}'", job_spec);

    // Log current job list for debugging
    debug!("FG_CMD_AVAILABLE_JOBS: Current job list:");
    for (i, job) in shell.wait_jobs.iter().enumerate() {
        debug!(
            "FG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
            i, job.job_id, job.pid, job.state, job.foreground, job.cmd
        );
    }

    let Some(job_index) = parse_job_spec(job_spec, &shell.wait_jobs) else {
        let error_msg = if job_spec.is_empty() {
            "fg: no current job".to_string()
        } else {
            format!("fg: job not found: {job_spec}")
        };
        debug!("FG_CMD_NOT_FOUND: {}", error_msg);
        ctx.write_stderr(&error_msg)?;
        return Err(anyhow::anyhow!(error_msg));
    };

    match run_foreground_driver(shell, ctx, job_index) {
        Ok(()) => Ok(()),
        Err(err) => {
            error!("FG_CMD_ERROR: foreground wait failed: {:?}", err);
            ctx.write_stderr(&format!("{err}")).ok();
            Err(err)
        }
    }
}

/// Sync → async bridge for job-control drivers (`fg`, `jobs`, `wait`).
///
/// The shell runs on a multi-thread Tokio runtime, so `block_in_place` hands
/// other tasks to a fresh worker while this thread blocks in `block_on`.
/// Never construct a nested `Runtime` here: that panics inside the existing
/// runtime. When no runtime exists (plain sync callers/tests), a fresh
/// runtime is created instead. `block_in_place` is multi-thread-only, so
/// unit tests must either call the async driver directly or run
/// on `#[tokio::test(flavor = "multi_thread")]`.
///
/// The runtime flavor is checked before entering `block_in_place`.
/// Current-thread runtimes are rejected without polling the foreground
/// future, so the job remains in `wait_jobs`. Internal panics inside the
/// future propagate as panics and are never converted into
/// runtime-compatibility errors.
pub(crate) fn run_foreground_driver(
    shell: &mut Shell,
    ctx: &Context,
    job_index: usize,
) -> Result<()> {
    block_on_job_control_future(foreground_selected_job(shell, ctx, job_index))?
}

/// Block a job-control future on the current Tokio runtime when possible.
///
/// Compatibility is decided up front via [`tokio::runtime::Handle::runtime_flavor`],
/// before the future is polled: multi-thread runtimes use
/// `block_in_place` + `block_on`, current-thread (and any unknown future
/// flavor) is rejected with an error, and callers outside any runtime get a
/// fresh `Runtime`. No `catch_unwind` is used here on purpose.
pub(crate) fn block_on_job_control_future<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                Ok(tokio::task::block_in_place(|| handle.block_on(future)))
            }
            tokio::runtime::RuntimeFlavor::CurrentThread => Err(anyhow::anyhow!(
                "job control requires a multi-thread Tokio runtime (current_thread is unsupported)"
            )),
            flavor => Err(anyhow::anyhow!(
                "job control does not support Tokio runtime flavor: {flavor:?}"
            )),
        },
        Err(_) => Ok(tokio::runtime::Runtime::new()?.block_on(future)),
    }
}

/// Async single source of truth for `fg` resume.
///
/// Takes temporary ownership of the job from `wait_jobs`, drives the existing
/// async foreground wait (which drains background `OutputMonitor`s), then
/// reconciles the lifecycle via [`finalize_foreground_job`]:
/// completed jobs are dropped, stopped/still-active jobs return to the table
/// with their real observed state.
pub(crate) async fn foreground_selected_job(
    shell: &mut Shell,
    ctx: &Context,
    job_index: usize,
) -> Result<()> {
    if job_index >= shell.wait_jobs.len() {
        return Err(anyhow::anyhow!("fg: no current job"));
    }
    let mut job = shell.wait_jobs.remove(job_index);
    debug!(
        "FG_CMD_SELECTED: Selected job {} at index {} for foreground",
        job.job_id, job_index
    );
    debug!(
        "FG_CMD_JOB_DETAILS: Job details before fg - state: {:?}, pgid: {:?}, pid: {:?}",
        job.state, job.pgid, job.pid
    );

    ctx.write_stdout(&format!(
        "dsh: job {} '{}' to foreground",
        job.job_id, job.cmd
    ))
    .ok();

    // Any stopped stage needs SIGCONT; the canonical tree (not the cached
    // `job.state` summary) decides so stale summaries cannot interfere.
    let cont = job.has_stopped_process();
    if cont {
        debug!(
            "FG_CMD_STOPPED: Job {} is stopped, will send SIGCONT",
            job.job_id
        );
    } else {
        debug!(
            "FG_CMD_NOT_STOPPED: Job {} is not stopped, no SIGCONT needed (state: {:?})",
            job.job_id, job.state
        );
    }

    // FullProxy resume preparation is transactional and must precede
    // SIGCONT / foreground wait: clone + writer failures leave the stopped
    // job intact so the caller can requeue it as `Stopped`.
    if crate::process::job_pty::uses_full_pty_proxy(&job)
        && let Err(err) = crate::process::job_pty::resume_pty_input_proxy(&mut job).await
    {
        job.refresh_lifecycle_state();
        shell.wait_jobs.push(job);
        return Err(err);
    }
    // Raw mode is scoped to each active FullProxy foreground interval:
    // enable → input proxy resume (above) → SIGCONT / foreground wait →
    // restore on guard drop (after finalization below).
    let _pty_raw_guard = crate::process::job_pty::ForegroundPtyRawModeGuard::for_resume(&job);

    let old_state = job.state;
    // `job.foreground` stays `false`: it records background provenance, not
    // temporary foreground ownership during this resume.
    job.state = ProcessState::Running;
    debug!(
        "FG_CMD_STATE_CHANGE: Set job {} state from {:?} to Running",
        job.job_id, old_state
    );

    debug!(
        "FG_CMD_FOREGROUND_CALL: About to call put_in_foreground for job {} with no_hang=true, cont={}",
        job.job_id, cont
    );

    // `fg` resumes a job whose own `job.foreground` stayed `false`
    // (it was backgrounded), so `yield_to_foreground_agent` can't
    // read that off `job` the way `shell/eval.rs` does for a
    // command that started in the foreground - pass `true`
    // directly instead. Without this, Ctrl+Z on a recognized agent
    // CLI followed by `fg` would leave this pane stuck reporting
    // `dsh` for the rest of that agent's run. The guard must cover the
    // whole await below.
    let _agent_handoff =
        crate::agent_lifecycle::yield_to_foreground_agent(shell, &job, ctx.interactive, true);

    let job_id = job.job_id;
    let wait_result = job.put_in_foreground(true, cont).await;
    debug!(
        "FG_CMD_WAIT_DONE: put_in_foreground finished for job {}: {:?}",
        job_id,
        wait_result.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    finalize_foreground_job(shell, job, wait_result, ctx).await
}

/// Reconcile a foreground wait and requeue the job when it is still active.
///
/// Refreshes the summary state from the process tree first (real `Stopped`
/// state, never synthesized), then pushes back to `wait_jobs` unless the
/// tree says completed. Retention runs before `wait_result` propagation so
/// an error path never orphans an active process group; a completed job is
/// still dropped even when the wait errored.
/// Finalize a foreground wait: completed trees go through PTY-specific
/// completion (input stop, output drain to EOF, history once) and then the
/// canonical completed-job finalizer (output drain to EOF, known-async
/// status archive); anything else returns to the job table with stopped
/// PTY ownership (`pty`/`pty_output_task` kept, input proxy stopped) so the
/// prompt never shares terminal input with a stopped FullProxy job.
///
/// Async because the canonical finalizer drains output monitors.
pub(crate) async fn finalize_foreground_job(
    shell: &mut Shell,
    mut job: crate::process::Job,
    wait_result: Result<()>,
    ctx: &Context,
) -> Result<()> {
    job.refresh_lifecycle_state();
    // Strict ownership: only a fully-completed tree may be dropped, and
    // only through the canonical finalizer.
    if !job.is_process_tree_completed() {
        // Returning to the prompt with a FullProxy job alive: stop the
        // input proxy (keep PTY/output) even on wait errors, so the REPL
        // never competes for `/dev/tty` reads. Never await output here: a
        // stopped child holds the slave, so EOF may never arrive.
        if crate::process::job_pty::uses_full_pty_proxy(&job) {
            crate::process::job_pty::suspend_stopped_pty_input(&mut job).await;
        }
        debug!(
            "FG_CMD_REQUEUE: Job {} still active after foreground wait (state: {:?}), returning to job table",
            job.job_id, job.state
        );
        shell.wait_jobs.push(job);
        return wait_result;
    }
    debug!(
        "FG_CMD_DONE: Job {} completed, finalizing through the canonical path",
        job.job_id
    );
    // PTY-specific completion first (shared with the initial foreground
    // launch path, not a duplicate algorithm), then the canonical
    // monitor/ledger finalizer. Both are attempted even when the wait
    // errored so a completed tree is never orphaned for error reporting.
    let has_pty_state =
        job.pty.is_some() || job.pty_output_task.is_some() || job.pty_mode.is_some();
    let pty_settlement = if has_pty_state {
        crate::process::job_pty::capture_completed_output_and_history(&mut job, ctx, shell).await
    } else {
        Ok(())
    };
    let finalizer_outcome = crate::shell::job::finalize_completed_job(
        shell,
        job,
        crate::shell::job::FinalizeDrain::ToEof,
    )
    .await;
    // Error precedence: foreground wait / process ownership first, PTY
    // settlement second. Reconciliation is never skipped for reporting.
    if let Err(wait_err) = wait_result {
        if let Err(pty_err) = &pty_settlement {
            debug!("fg PTY settlement also failed: {pty_err:#}");
        }
        if let Err(fin_err) = &finalizer_outcome {
            debug!("fg canonical finalizer also failed: {fin_err:#}");
        }
        return Err(wait_err);
    }
    pty_settlement?;
    finalizer_outcome.map(|_| ())
}

/// Reconcile a background resume without orphaning an active job on error.
///
/// Async because an already-completed tree leaves through the canonical
/// finalizer (non-blocking `ReadyNow` retirement: `bg` must return to the
/// prompt even when a descendant holds the pipe).
pub(crate) async fn finalize_background_resume(
    shell: &mut Shell,
    mut job: crate::process::Job,
    resume_result: Result<()>,
) -> Result<()> {
    if resume_result.is_ok() {
        job.mark_stopped_processes_running();
    } else {
        job.refresh_lifecycle_state();
    }

    if !job.is_process_tree_completed() {
        debug!(
            "BG_CMD_REQUEUE: Job {} remains active after resume attempt (state: {:?})",
            job.job_id, job.state
        );
        shell.wait_jobs.push(job);
    } else {
        debug!(
            "BG_CMD_DONE: Job {} is already completed, finalizing through the canonical path",
            job.job_id
        );
        crate::shell::job::finalize_completed_job(
            shell,
            job,
            crate::shell::job::FinalizeDrain::ReadyNow,
        )
        .await?;
    }

    resume_result
}
