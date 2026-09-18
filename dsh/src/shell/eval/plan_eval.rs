//! Shared `ExecutionPlan` evaluator for isolated (helper) execution.
//!
//! The order mirrors the top-level `eval_str` loop exactly — gate on
//! `&&`/`||` first, materialize only the selected job (per-job runtime word
//! expansion), authorize the final concrete argv through `SafetyGuard`, then
//! launch and publish the status for the next job — minus everything that
//! belongs to the interactive session: no hooks, no title, no agent handoff,
//! no `|>` capture or `|:` struct-pipe branches (a nested job's own `launch`
//! already wires its stdio; capture flags on nested jobs behave as they did
//! in the old in-process substitution loop, i.e. output flows to the
//! helper's stdout).
//!
//! Both the top-level shell and every re-exec helper judge nested dynamic
//! commands through the same funnel, so `$(...)` and `<(...)` bodies cannot
//! bypass the policy the interactive line enforces.

use super::super::Shell;
use super::super::authorize::{
    AuthorizationCancelled, AuthorizationDecision, ConfirmFn, authorize_job_with,
};
use super::super::materialize::materialize_job;
use super::super::plan::ExecutionPlan;
use anyhow::Result;
use dsh_types::Context;
use tracing::debug;

/// Run every selected job of `plan` to completion. Returns the last exit
/// status. A nested denial aborts with `AuthorizationCancelled`, exactly like
/// the top-level loop.
///
/// Jobs run sequentially and each completion steers the next gate, so inner
/// `&&`/`||` behave exactly as on the top level. An infinite producer tail
/// (`<(yes)`) blocks its helper the way it would block any shell — the
/// parent never waits on producers; its group-kill reaper bounds their
/// lifetime instead.
pub(crate) async fn evaluate_plan(
    shell: &mut Shell,
    ctx: &mut Context,
    plan: &ExecutionPlan,
    confirm: ConfirmFn,
) -> Result<i32> {
    use crate::process::ListOp;
    use crate::process::ProcessState;

    let mut last_exit_code = 0_i32;
    let mut gate_op = ListOp::None;
    for planned in &plan.jobs {
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

        let materialized = match materialize_job(shell, ctx, planned, confirm).await? {
            Some(materialized) => materialized,
            None => {
                if planned.is_assignment_only() {
                    last_exit_code = 0;
                    super::publish_exit_status(shell, last_exit_code);
                }
                gate_op = next_gate_op;
                continue;
            }
        };
        let mut job = materialized.job;
        job.resources = materialized.resources;
        match authorize_job_with(shell, &job, materialized.had_dynamic_expansion, confirm)? {
            AuthorizationDecision::Allow => {}
            AuthorizationDecision::Deny => {
                anyhow::bail!(AuthorizationCancelled);
            }
        }

        job.job_id = shell.get_job_id();
        match job.launch(ctx, shell).await? {
            ProcessState::Running => {
                // A background job inside an isolated body is detached: the
                // helper cannot wait for it without outliving its purpose.
                // Record success-at-start like the top-level loop does.
                shell.wait_jobs.push(job);
                last_exit_code = 0;
            }
            ProcessState::Stopped(_, _) => {
                shell.wait_jobs.push(job);
                break;
            }
            state @ ProcessState::Completed(_, _) => {
                last_exit_code = state
                    .shell_exit_code()
                    .expect("completed state has exit code");
            }
        }
        super::publish_exit_status(shell, last_exit_code);
        gate_op = next_gate_op;
    }
    super::publish_exit_status(shell, last_exit_code);
    Ok(last_exit_code)
}
