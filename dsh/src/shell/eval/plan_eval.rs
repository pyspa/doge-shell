//! Shared `ExecutionPlan` evaluator for isolated (helper) execution.
//!
//! The order mirrors the top-level `eval_str` loop exactly — per AND-OR
//! list, gate on `&&`/`||` first, materialize only the selected job
//! (per-job runtime word expansion), authorize the final concrete argv
//! through `SafetyGuard`, then launch and publish the status for the next
//! job — minus everything that belongs to the interactive session: no
//! hooks, no title, no agent handoff, no `|>` capture or `|:` struct-pipe
//! branches (a nested job's own `launch` already wires its stdio; capture
//! flags on nested jobs behave as they did in the old in-process
//! substitution loop, i.e. output flows to the helper's stdout).
//!
//! Both the top-level shell and every re-exec helper judge nested dynamic
//! commands through the same funnel, so `$(...)` and `<(...)` bodies cannot
//! bypass the policy the interactive line enforces.

use super::super::Shell;
use super::super::authorize::{
    AuthorizationCancelled, AuthorizationDecision, ConfirmFn, authorize_job_with,
};
use super::super::materialize::{MaterializeOutcome, materialize_job};
use super::super::pipeline_isolation::reject_session_bound_background;
use super::super::plan::{ExecutionPlan, ListExecutionMode, PlannedAndOrList};
use crate::process::JobLaunchOutcome;
use anyhow::Result;
use dsh_types::Context;
use tracing::debug;

/// Which isolated body is being evaluated: only an async-list helper must
/// refuse session-bound builtins (its session is empty by construction).
/// Substitution and subshell helpers keep the historical in-process
/// behavior for the builtins they select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanEvaluationEnvironment {
    Isolated,
    AsyncList,
}

/// Run every selected list of `plan` to completion in `env`. Returns the
/// last exit status. A nested denial aborts with `AuthorizationCancelled`,
/// exactly like the top-level loop.
///
/// Jobs run sequentially and each completion steers the next gate, so inner
/// `&&`/`||` behave exactly as on the top level. An infinite producer tail
/// (`<(yes)`) blocks its helper the way it would block any shell — the
/// parent never waits on producers; its group-kill reaper bounds their
/// lifetime instead.
///
/// Async-list helpers pass [`PlanEvaluationEnvironment::AsyncList`] so a
/// *selected* session-bound builtin fails closed instead of reading the
/// helper's empty session.
pub(crate) async fn evaluate_plan_in(
    shell: &mut Shell,
    ctx: &mut Context,
    plan: &ExecutionPlan,
    confirm: ConfirmFn,
    env: PlanEvaluationEnvironment,
) -> Result<i32> {
    let mut last_exit_code = 0_i32;
    for list in &plan.lists {
        match list.execution {
            ListExecutionMode::Foreground => {
                last_exit_code = evaluate_foreground_list(shell, ctx, list, confirm, env).await?;
            }
            ListExecutionMode::Asynchronous => {
                // A nested async list (e.g. inside `$(...)`): spawn a nested
                // helper joining this helper's process group, then continue
                // without waiting — exactly like the top level.
                spawn_nested_async_list(shell, ctx, list).await?;
                last_exit_code = 0;
                super::publish_exit_status(shell, last_exit_code);
            }
        }
    }
    super::publish_exit_status(shell, last_exit_code);
    Ok(last_exit_code)
}

/// Evaluate one foreground AND-OR list: gate, materialize, authorize,
/// launch. The gate resets per list — a `;`/`&` boundary never carries the
/// previous list's `ListOp` forward.
async fn evaluate_foreground_list(
    shell: &mut Shell,
    ctx: &mut Context,
    list: &PlannedAndOrList,
    confirm: ConfirmFn,
    env: PlanEvaluationEnvironment,
) -> Result<i32> {
    use crate::process::ListOp;
    use crate::process::ProcessState;

    let mut last_exit_code = 0_i32;
    let mut gate_op = ListOp::None;
    for planned in &list.jobs {
        let next_gate_op = planned.list_op.clone();
        let should_run = match gate_op {
            ListOp::None => true,
            ListOp::And => last_exit_code == 0,
            ListOp::Or => last_exit_code != 0,
        };
        if !should_run {
            debug!(
                "helper skips job '{}' due to gate_op:{:?} last_exit_code:{}",
                planned.source, gate_op, last_exit_code
            );
            gate_op = next_gate_op;
            continue;
        }

        // Mirrors the top-level loop: a rejected builtin prefix is an
        // ordinary command failure (publish + continue), not an abort.
        // No-command stages run through the same shared executor as the top
        // level, so `&&`/`||` gating cannot drift between parent and helper.
        let materialized = match materialize_job(shell, ctx, planned, confirm).await? {
            MaterializeOutcome::Runnable(materialized) => materialized,
            MaterializeOutcome::NoCommand(no_command) => {
                use super::super::no_command::{NoCommandExecutionResult, execute_no_command};
                match execute_no_command(shell, ctx, *no_command) {
                    NoCommandExecutionResult::Completed(code) => {
                        last_exit_code = code;
                    }
                    NoCommandExecutionResult::Failed(failure) => {
                        let _ = ctx.write_stderr(&failure.message);
                        last_exit_code = failure.exit_code;
                    }
                }
                super::publish_exit_status(shell, last_exit_code);
                gate_op = next_gate_op;
                continue;
            }
            MaterializeOutcome::Rejected(failure) => {
                let _ = ctx.write_stderr(&failure.message);
                last_exit_code = failure.exit_code;
                super::publish_exit_status(shell, last_exit_code);
                gate_op = next_gate_op;
                continue;
            }
        };
        let mut job = materialized.job;
        job.resources = materialized.resources;
        // Async helpers own no live session: a *selected* session-bound
        // builtin fails closed here. Gated-out jobs never reach this point,
        // so `false && jobs &` stays silent. Pipelines already passed the
        // whole-pipeline preflight during materialization.
        if env == PlanEvaluationEnvironment::AsyncList
            && let Err(failure) = reject_session_bound_background(&job, shell)
        {
            let _ = ctx.write_stderr(&failure.message);
            last_exit_code = failure.exit_code;
            super::publish_exit_status(shell, last_exit_code);
            gate_op = next_gate_op;
            continue;
        }
        match authorize_job_with(shell, &job, materialized.had_dynamic_expansion, confirm)? {
            AuthorizationDecision::Allow => {}
            AuthorizationDecision::Deny => {
                anyhow::bail!(AuthorizationCancelled);
            }
        }

        job.job_id = shell.get_job_id();
        // A redirection setup failure is an ordinary command failure here
        // too: report it on the helper's stderr, publish status 1 for the
        // next gate, and continue the plan. It must never become a helper
        // protocol infrastructure error.
        match job.launch(ctx, shell).await? {
            JobLaunchOutcome::Process(ProcessState::Running) => {
                // A background job inside an isolated body is detached: the
                // helper cannot wait for it without outliving its purpose.
                // Record success-at-start like the top-level loop does.
                shell.wait_jobs.push(job);
                last_exit_code = 0;
            }
            JobLaunchOutcome::Process(ProcessState::Stopped(_, _)) => {
                shell.wait_jobs.push(job);
                break;
            }
            JobLaunchOutcome::Process(state @ ProcessState::Completed(_, _)) => {
                last_exit_code = state
                    .shell_exit_code()
                    .expect("completed state has exit code");
            }
            JobLaunchOutcome::CommandFailed(failure) => {
                let _ = ctx.write_stderr(&failure.message);
                last_exit_code = failure.exit_code;
            }
        }
        super::publish_exit_status(shell, last_exit_code);
        gate_op = next_gate_op;
    }
    super::publish_exit_status(shell, last_exit_code);
    Ok(last_exit_code)
}

/// Spawn one nested async list from inside a helper.
///
/// The nested helper joins this helper's process group (`ctx.pgid` is the
/// outer helper's pid by the time we run here), so the parent's group-kill
/// reaper still reaches the whole tree. The job is tracked on the helper's
/// own `wait_jobs`; the helper never waits for it here.
async fn spawn_nested_async_list(
    shell: &mut Shell,
    ctx: &mut Context,
    list: &PlannedAndOrList,
) -> Result<()> {
    use crate::process::{AsyncListProcess, Job, JobProcess};

    let source = list.display_source();
    let mut job = Job::new(source.clone(), shell.pgid);
    job.job_id = shell.get_job_id();
    job.foreground = false;
    job.set_process(JobProcess::AsyncList(AsyncListProcess::new(
        source.clone(),
        list.isolated_body_plan(),
    )));
    // `Job::launch` snapshots base stdio and restores `ctx` on the way
    // out; the helper's own stdio is untouched after this returns.
    let outcome = job.launch(ctx, shell).await?;
    debug!("nested async list '{source}' launched: {outcome:?}");
    match outcome {
        // A nested async launch registers `$!` and wait ownership on the
        // helper's own table, exactly like the top level — but the ledger
        // stays local and never propagates to the outer parent.
        crate::process::JobLaunchOutcome::Process(_) => {
            shell.track_async_job(job)?;
        }
        // A spawn-level failure is an ordinary command failure, like the
        // top-level paths: report it without touching `$!` or the ledger.
        crate::process::JobLaunchOutcome::CommandFailed(failure) => {
            let _ = ctx.write_stderr(&failure.message);
        }
    }
    Ok(())
}
