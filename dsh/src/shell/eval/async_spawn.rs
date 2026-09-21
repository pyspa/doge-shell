//! Parent-side spawns for asynchronous AND-OR lists.
//!
//! `&` separates whole AND-OR lists, so the parent runs one fresh `dogesh`
//! helper per async list (or one for the whole line on the REPL
//! `force_background` shortcut) and immediately continues with the next
//! list. The helper body failure never rewrites the parent's launch status:
//! a successful spawn reports 0.

use super::TitleGuard;
use crate::process::{AsyncListProcess, Job, JobLaunchOutcome, JobProcess, ProcessState};
use crate::shell::Shell;
use crate::shell::plan::{ExecutionPlan, PlannedAndOrList};
use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_types::Context;
use tracing::debug;

/// Spawn one asynchronous AND-OR list as a managed background job.
///
/// Returns 0 when the helper launched: the parent's visible status for the
/// async launch itself, independent of whatever the helper body later
/// reports through the job table.
pub(super) async fn spawn_async_list_job(
    shell: &mut Shell,
    ctx: &mut Context,
    list: &PlannedAndOrList,
) -> Result<i32> {
    let source = list.display_source();

    // Execute pre-exec hooks for the async launch, like any other job.
    // There is no post-exec hook: completion is asynchronous and the job
    // table/notices own that lifecycle.
    if let Err(e) = shell.exec_pre_exec_hooks(&source) {
        debug!("Error executing pre-exec hooks: {}", e);
    }

    // Cooked mode for the spawn, mirroring foreground launches.
    if let Err(e) = disable_raw_mode() {
        debug!("EVAL_STR: Failed to disable raw mode: {}", e);
    }

    let mut job = Job::new(source.clone(), shell.pgid);
    job.job_id = shell.get_job_id();
    job.foreground = false;
    job.set_process(JobProcess::AsyncList(AsyncListProcess::new(
        source.clone(),
        list.isolated_body_plan(),
    )));

    debug!("start async list '{source}'");
    let _title_guard = TitleGuard::new(ctx, &job);
    let outcome = job.launch(ctx, shell).await?;
    debug!("async list '{source}' launch outcome: {outcome:?}");

    // Re-enable raw mode after the spawn (only in interactive mode).
    if ctx.interactive {
        enable_raw_mode().ok();
    }

    match outcome {
        // The helper is running: success-at-start, never the body status.
        JobLaunchOutcome::Process(ProcessState::Running) => {
            shell.wait_jobs.push(job);
            Ok(0)
        }
        JobLaunchOutcome::Process(state) => {
            // The helper already finished (fast body): still a successful
            // launch from the parent's point of view; reconcile through
            // the job table like any completed background job.
            debug!("async list '{source}' finished during launch: {state:?}");
            shell.wait_jobs.push(job);
            Ok(0)
        }
        JobLaunchOutcome::CommandFailed(failure) => {
            // Spawn-level failure (never a body result): report it like an
            // ordinary command failure.
            let _ = ctx.write_stderr(&failure.message);
            Ok(failure.exit_code)
        }
    }
}

/// Run the whole plan in one background helper (the `force_background`
/// path, e.g. the REPL shortcut that backgrounds the entire line).
pub(super) async fn spawn_whole_plan_background(
    shell: &mut Shell,
    ctx: &mut Context,
    plan: &ExecutionPlan,
    input: &str,
    base_infile: std::os::unix::io::RawFd,
    base_outfile: std::os::unix::io::RawFd,
    base_errfile: std::os::unix::io::RawFd,
) -> Result<i32> {
    ctx.infile = base_infile;
    ctx.outfile = base_outfile;
    ctx.errfile = base_errfile;

    let source = input.trim().to_string();
    if let Err(e) = shell.exec_pre_exec_hooks(&source) {
        debug!("Error executing pre-exec hooks: {}", e);
    }
    if let Err(e) = disable_raw_mode() {
        debug!("EVAL_STR: Failed to disable raw mode: {}", e);
    }

    // The helper evaluates the plan's own list structure (explicit `;`,
    // `&&`, `||`, `&` included) in one shell, so multi-job lines share a
    // single isolated environment while an explicit `&` still spawns a
    // nested helper inside it.
    let mut job = Job::new(source.clone(), shell.pgid);
    job.job_id = shell.get_job_id();
    job.foreground = false;
    job.set_process(JobProcess::AsyncList(AsyncListProcess::new(
        source.clone(),
        plan.clone(),
    )));

    debug!("start whole-plan background '{source}'");
    let outcome = job.launch(ctx, shell).await?;
    if ctx.interactive {
        enable_raw_mode().ok();
    }
    match outcome {
        JobLaunchOutcome::Process(_) => {
            shell.wait_jobs.push(job);
            Ok(0)
        }
        JobLaunchOutcome::CommandFailed(failure) => {
            let _ = ctx.write_stderr(&failure.message);
            Ok(failure.exit_code)
        }
    }
}
