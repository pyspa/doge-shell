//! `agent approve`: the one-command answer to a grant-stuck task.
//!
//! `agent show --summary` already prints the exact resume line a stuck task
//! needs, but acting on it means copying a byte-exact `--allow-command` or
//! `--allow-mcp` value (quoting included) by hand. `approve` reads the same
//! refusal the summary names, asks once, widens the grant, and resumes -
//! foreground only, one refusal at a time, so every widening stays an
//! explicit, reviewable human decision.
//!
//! The refusal source is shared with the summary (`blocked::is_grant_hint`
//! for the recorded reason, `blocked::denial_hint_from_events` for the
//! event log), and the key taxonomy with `blocked_need`
//! (`blocked::classify_approval_key`), so the three can never disagree
//! about what a refusal means.

use super::blocked::{
    COMMAND_CONFIRM_SUFFIXES, approval_key, classify_approval_key, denial_hint_from_events,
    is_grant_hint, is_runnable_cron_key, mcp_grant_entry,
};
use super::{SqliteTaskStore, locks};
use anyhow::{Context as _, Result, bail};
use dsh_builtin::shell_capabilities::AgentTaskStore as _;
use dsh_types::{
    Context,
    agent::{AgentTask, TaskEvent, TaskStatus},
};
use serde_json::json;
use std::sync::Arc;

/// One grant to add before resuming: the `apply_grant_option` flag plus its
/// value, and the human description of what was refused.
pub(crate) struct GrantApproval {
    pub option: &'static str,
    pub value: String,
    pub what: String,
}

/// What the last refusal of a stopped task resolves to.
pub(crate) enum ApprovalResolution {
    /// One grant to add, then resume.
    Grantable(GrantApproval),
    /// Refused, but no flag can satisfy it: `guidance` tells the person
    /// what to do instead (act themselves, then resume with `--reconcile`).
    Ungrantable { what: String, guidance: String },
    /// No grant-shaped refusal on record at all.
    NothingFound { diagnostic: String },
}

/// Resolves the last refusal of `task` (plus its `events`) into at most one
/// grant approval.
///
/// Pure: takes the same inputs the summary's `needs:` line reads, so a fix
/// shown there and an approval offered here always agree. Callers handle
/// `pending_operation` themselves - an unreconciled operation blocks any
/// resume no matter what grant is added.
pub(crate) fn resolve_approval(task: &AgentTask, events: &[TaskEvent]) -> ApprovalResolution {
    let hint = task
        .stop_reason
        .as_deref()
        .filter(|reason| is_grant_hint(reason))
        .map(str::to_string)
        .or_else(|| denial_hint_from_events(events));
    let Some(hint) = hint else {
        return ApprovalResolution::NothingFound {
            diagnostic: match (&task.status, &task.stop_reason) {
                (TaskStatus::InputRequired, Some(reason)) => format!(
                    "task {} waits on a person, but its stop reason names no grant ({reason}); inspect with `agent show {}`",
                    task.id, task.id
                ),
                (TaskStatus::InputRequired, None) => format!(
                    "task {} waits on a person for an unknown reason; inspect with `agent show {}`",
                    task.id, task.id
                ),
                (status, _) => format!(
                    "task {} is not stuck on a grant (status: {}); inspect with `agent show {}`",
                    task.id,
                    super::summary::status_label(*status),
                    task.id
                ),
            },
        };
    };

    if let Some(key) = approval_key(&hint) {
        let what = hint
            .split(" [approval_key:")
            .next()
            .unwrap_or(&hint)
            .to_string();
        return match classify_approval_key(key) {
            super::blocked::ApprovalKeyKind::WriteDir(dir) => {
                ApprovalResolution::Grantable(GrantApproval {
                    option: "--write",
                    value: dir,
                    what,
                })
            }
            super::blocked::ApprovalKeyKind::Mcp(entry) => {
                ApprovalResolution::Grantable(GrantApproval {
                    option: "--allow-mcp",
                    value: entry,
                    what,
                })
            }
            super::blocked::ApprovalKeyKind::Cron(action, job) => {
                let guidance = if is_runnable_cron_key(&action) {
                    format!(
                        "run `cron {action} {job}` yourself, then resume the task:\n  agent resume {} --reconcile 'describe what the cron change did'",
                        task.id
                    )
                } else {
                    format!(
                        "this cron change ({action}) cannot be granted; act on the job yourself, then resume the task:\n  agent resume {} --reconcile 'describe what you did'",
                        task.id
                    )
                };
                ApprovalResolution::Ungrantable { what, guidance }
            }
            super::blocked::ApprovalKeyKind::Other => ApprovalResolution::Ungrantable {
                what,
                guidance: format!(
                    "this refusal ({hint}) cannot be satisfied by any grant flag; act on it yourself, then resume the task:\n  agent resume {} --reconcile 'describe what you did'",
                    task.id
                ),
            },
        };
    }

    if let Some(entry) = mcp_grant_entry(&hint) {
        return ApprovalResolution::Grantable(GrantApproval {
            option: "--allow-mcp",
            value: entry.to_string(),
            what: hint,
        });
    }

    for suffix in COMMAND_CONFIRM_SUFFIXES {
        if let Some(command) = hint.strip_suffix(suffix) {
            return ApprovalResolution::Grantable(GrantApproval {
                option: "--allow-command",
                value: command.to_string(),
                what: format!("permission to run `{command}`"),
            });
        }
    }

    // Reached only for shapes no flag can satisfy (notably
    // `UNGRANTABLE_COMMAND_SUFFIXES`, e.g. a skill script): report, never
    // offer.
    ApprovalResolution::Ungrantable {
        what: hint.clone(),
        guidance: format!(
            "this refusal cannot be satisfied by any grant flag; act on it yourself, then resume the task:\n  agent resume {} --reconcile 'describe what you did'",
            task.id
        ),
    }
}

/// Renders one grant as the resume flag that would add it by hand - the
/// same spelling `blocked_need`'s fix lines use.
fn resume_flag(option: &str, value: &str) -> String {
    if option == "--write" {
        format!("{option} {value}")
    } else {
        format!("{option} '{value}'")
    }
}

/// Whether `grant` already contains `value` for `option` - approving twice
/// must be a no-op with a pointer, not a duplicate entry.
fn already_granted(task: &AgentTask, option: &str, value: &str) -> bool {
    match option {
        "--write" => task
            .grant
            .write_roots
            .iter()
            .any(|root| root.to_string_lossy() == value),
        "--allow-command" => task.grant.commands.iter().any(|c| c == value),
        "--allow-mcp" => task.grant.mcp_calls.iter().any(|m| m == value),
        _ => false,
    }
}

/// `agent approve ID [--reconcile TEXT] [--dry-run]`: ask once, widen the
/// grant by the one refusal the task is stuck on, and resume in the
/// foreground.
///
/// Foreground-only by design: the confirmation happens here, in the shell
/// that runs this command. A detached continuation stays `agent resume ID
/// --detach`'s job once the grant is in place.
pub(crate) fn approve(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &Arc<SqliteTaskStore>,
    args: &[String],
) -> Result<()> {
    let id = args.first().context("task ID required")?;
    let mut reconcile = None;
    let mut dry_run = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--reconcile" => {
                index += 1;
                reconcile = Some(args.get(index).context("option value required")?.clone());
                index += 1;
            }
            other => bail!("unsupported option {other}"),
        }
    }

    // Held for the whole approval: a second shell must not widen the same
    // task's grant (or resume it) underneath this one's confirmation.
    let _lock = locks::try_lock_task(store, id)?
        .context("this task is currently running; cancel it or wait")?;
    let mut task = store.load(id)?;
    if task.status == TaskStatus::Running {
        bail!("task {id} is already running; `agent wait`/`agent cancel` it before approving");
    }
    if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
        bail!("task {id} already finished ({:?}); start a new task with `agent run`", task.status);
    }
    if task.pending_operation.is_some() && reconcile.is_none() {
        bail!(
            "previous operation has an unknown outcome; inspect `agent show {id}` and the actual files/service, then approve with --reconcile describing the observed result"
        );
    }
    if task.elapsed_ms >= task.time_budget_ms {
        bail!(
            "task {id} has no time left; resume with a larger timeout first (`agent resume {id} --timeout SECONDS`), then approve if it gets stuck on a grant"
        );
    }
    let events = store.events(id)?;
    let approval = match resolve_approval(&task, &events) {
        ApprovalResolution::Grantable(approval) => approval,
        ApprovalResolution::Ungrantable { what, guidance } => {
            ctx.write_stdout(&format!("cannot approve: {what}\n{guidance}"))?;
            bail!("no grantable permission for task {id}");
        }
        ApprovalResolution::NothingFound { diagnostic } => {
            bail!("{diagnostic}");
        }
    };

    let resume_hint = format!("agent resume {id} {}", resume_flag(approval.option, &approval.value));
    if already_granted(&task, approval.option, &approval.value) {
        ctx.write_stdout(&format!(
            "already granted: `{}` {}\nThe task is stuck on something else; inspect with `agent show {id} --summary`.",
            approval.option, approval.value
        ))?;
        return Ok(());
    }
    if dry_run {
        ctx.write_stdout(&format!(
            "would add grant `{}` {} for task {id} ({})\nthen: {resume_hint}",
            approval.option, approval.value, approval.what
        ))?;
        return Ok(());
    }
    match crate::repl::confirmation::confirm_action(&format!(
        "Task {id} was refused: {}\nAdd grant `{}` {} and resume now?",
        approval.what, approval.option, approval.value
    )) {
        // `AlwaysAllow` answers the one question on screen: it is scoped to
        // this approval only and never remembered anywhere.
        Ok(crate::repl::confirmation::ConfirmationAction::Yes)
        | Ok(crate::repl::confirmation::ConfirmationAction::AlwaysAllow) => {}
        Ok(crate::repl::confirmation::ConfirmationAction::No) => {
            ctx.write_stdout(&format!("not approved; when ready, resume by hand:\n  {resume_hint}"))?;
            return Ok(());
        }
        Err(error) => bail!("confirmation failed: {error:#}"),
    }
    // `AllowAlways` above is deliberately scoped to this one approval: an
    // "always" remembered here would silently widen every later refusal,
    // which is exactly what task grants exist to prevent.
    dsh_builtin::agent::grant::apply_grant_option(&mut task.grant, approval.option, &approval.value)?;
    store.save(
        &task,
        Some((
            "grants_extended",
            &json!({"added": [{"option": approval.option, "value": approval.value}]}),
        )),
    )?;
    // No `report_idle` here (unlike cancel/delete): a declined or failed
    // approval leaves the task stuck, and this shell's lifecycle state must
    // keep saying so. A resumed run manages the lifecycle itself.
    super::admit_and_run(shell, ctx, store, task, reconcile)
}

#[cfg(test)]
mod tests;
