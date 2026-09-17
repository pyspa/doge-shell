//! Turning a stopped task's `stop_reason` into the one command line that
//! would unblock it, instead of making a person re-derive it from
//! `agent show`'s full JSON dump.
//!
//! `stop_reason` already carries everything needed for the common cases:
//! `confirm_agent_action_with_preview` (`dsh-builtin/src/chatgpt/tool/safety_gates.rs`)
//! folds the approval key itself into the message as `[approval_key: X]` for
//! file writes, `cron_manage` and hooks; `Shell::evaluate_agent_command`'s
//! `Confirm` path (`dsh/src/proxy/agent_policy.rs`) is reported as
//! `"{command}: {reason}"` for a plain command. An MCP call is the odd one
//! out: `authorize_mcp_tool` (`dsh-builtin/src/chatgpt/tool/mod.rs`) calls
//! `request_agent_approval` directly rather than going through
//! `confirm_agent_action_with_preview`, so its `stop_reason` never gets an
//! `[approval_key: ...]` marker at all - it is matched on its own literal
//! shape instead (see [`mcp_grant_entry`]). Nothing here needs a second,
//! independent source of truth (re-reading `tool_intent` events) - the
//! message a person already sees in `agent show`/`cron logs` is the
//! parseable one.

use dsh_types::agent::{AgentTask, TaskEvent};
use serde_json::Value;

/// What is missing, and the exact line that would supply it.
pub(crate) struct BlockedNeed {
    pub(crate) what: String,
    /// A command the user can type verbatim - never a guess dressed up as
    /// one; see [`blocked_need`]'s `None` case for when there is nothing
    /// exact to offer.
    pub(crate) fix: Option<String>,
}

/// Reasons `Shell::evaluate_agent_command` (`dsh/src/proxy/agent_policy.rs`)
/// gives for a `Confirm` verdict on a plain command, exactly as they appear
/// after the command line in `stop_reason` (`"{command}: {reason}"`).
/// Only reasons an `agent resume --allow-command` can actually satisfy:
/// anything else the policy reports (notably the skill-script rule below,
/// which `execute::authorize` checks before grants are even consulted) must
/// not be offered as a flag that would fail the instant it runs.
pub(crate) const COMMAND_CONFIRM_SUFFIXES: &[&str] =
    &[": command is not in the task's exact command grants"];

/// Command-shaped refusals no resume flag can satisfy. A skill script is
/// checked before grants (`execute::authorize`'s skill-script branch runs
/// first, and profiles never bypass it), so suggesting `--allow-command`
/// for one would fail the instant a person ran it - the same reason
/// `hook:`/`sensitive:`/`delete:` keys resolve to no fix.
pub(crate) const UNGRANTABLE_COMMAND_SUFFIXES: &[&str] =
    &[": running a skill script needs its own approval"];

/// The text `evaluate_agent_tool` (`dsh/src/proxy/agent_policy.rs`) gives for
/// an MCP call outside the task's grant, exactly as `authorize_mcp_tool`
/// (`dsh-builtin/src/chatgpt/tool/mod.rs`) wraps it into `stop_reason`:
/// `"AI wants to call MCP tool: `NAME` ({this}{entry})"`. `entry` is already
/// the exact `mcp:tool:args` string `--allow-mcp` expects
/// (`SafetyGuard::mcp_allowlist_entry`), so it is pulled out of the plain
/// message text rather than an `[approval_key: ...]` marker - see this
/// module's doc comment for why MCP has no marker to match on instead.
const MCP_GRANT_REASON_PREFIX: &str = "external operation needs an exact task grant: ";

/// Recovers the latest grant refusal from the event log when the task's own
/// `stop_reason` carries no parseable hint.
///
/// Only the newest `tool_result` counts - mirroring `AgentRuntime`'s
/// in-memory rule (a later non-denial result clears the hint): an older
/// refusal the task already moved past must not surface as the resume fix.
/// Returns a canonical hint in exactly the shapes [`blocked_need`] parses.
/// `None` when the latest failure names no grant, or the latest result is
/// not a failure at all.
pub(crate) fn denial_hint_from_events(events: &[TaskEvent]) -> Option<String> {
    let event = events.iter().rev().find(|event| event.kind == "tool_result")?;
    if event.data.get("failed").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let result = event.data.get("result").and_then(Value::as_str)?;
    // File/skill/cron/hook writes: the `[approval_key: ...]` marker survives
    // trailing boilerplate, so rebuild the canonical shape around it.
    if let Some(key) = approval_key(result) {
        if let Some(message) = approval_message(result) {
            return Some(format!("{message} [approval_key: {key}]"));
        }
        return Some(format!("[approval_key: {key}]"));
    }
    // Exact commands and skill scripts: the result names the command but not
    // the policy reason, so rebuild it from the matching intent (exact) or,
    // when the intent is missing, from the result's own first line.
    for (prefix, suffix) in [
        (
            "agent: command permission required: ",
            ": command is not in the task's exact command grants",
        ),
        (
            "agent: skill script permission required: ",
            ": running a skill script needs its own approval",
        ),
    ] {
        if !result.contains(prefix) {
            continue;
        }
        let command = intent_command(events, event).or_else(|| {
            let start = result.find(prefix)? + prefix.len();
            let command = result[start..].lines().next()?.trim();
            (!command.is_empty()).then_some(command.to_string())
        })?;
        return Some(format!("{command}{suffix}"));
    }
    // MCP denials report as a cancellation result, so rebuild the canonical
    // message from the matching intent's exact entry (byte-identical:
    // `mcp_allowlist_entry` is deterministic for the same arguments).
    if result.contains("MCP tool execution cancelled by user.") {
        return mcp_hint_from_intent(events, event);
    }
    None
}

/// The human message ahead of an `[approval_key: ...]` marker in a denial
/// result (`"Error: agent: permission required: {message} [approval_key:
/// ...]\nPlease analyze..."`).
fn approval_message(result: &str) -> Option<&str> {
    let start = result.find("agent: permission required: ")? + "agent: permission required: ".len();
    let rest = &result[start..];
    let end = rest.find(" [approval_key:")?;
    Some(rest[..end].trim_end())
}

/// The `tool_intent` call matching a `tool_result`'s recorded call id.
fn intent_for<'a>(events: &'a [TaskEvent], event: &TaskEvent) -> Option<&'a Value> {
    let call_id = event.data.get("call")?.get("id")?.as_str()?;
    events.iter().rev().find_map(|intent| {
        if intent.kind != "tool_intent" {
            return None;
        }
        (intent.data.get("id").and_then(Value::as_str) == Some(call_id)).then_some(&intent.data)
    })
}

/// The exact command line behind an `execute` refusal, from its intent.
fn intent_command(events: &[TaskEvent], event: &TaskEvent) -> Option<String> {
    let intent = intent_for(events, event)?;
    let args = intent.get("function")?.get("arguments")?.as_str()?;
    serde_json::from_str::<Value>(args)
        .ok()?
        .get("command")?
        .as_str()
        .map(str::to_string)
}

/// The canonical MCP refusal message, rebuilt from its intent's exact entry.
fn mcp_hint_from_intent(events: &[TaskEvent], event: &TaskEvent) -> Option<String> {
    let intent = intent_for(events, event)?;
    let function = intent.get("function")?;
    let name = function.get("name")?.as_str()?;
    let arguments = function.get("arguments")?.as_str()?;
    let entry = crate::safety::SafetyGuard::mcp_allowlist_entry(name, arguments);
    Some(format!(
        "AI wants to call MCP tool: `{name}` (external operation needs an exact task grant: {entry})"
    ))
}

/// Whether `reason` names a missing grant - the shapes the approval gates
/// record via `AgentRuntime::note_denial` (and, before deny-and-continue,
/// wrote into an `InputRequired` task's `stop_reason` directly).
///
/// Shared by [`blocked_need`] (which turns a stopped task into a resume
/// command) and cron's `agent_run_outcome` (which must tell a grant-stuck
/// `Interrupted` run from a timed-out one: only the former earns a
/// `NeedsApproval` incident instead of another fresh retry).
pub(crate) fn is_grant_hint(reason: &str) -> bool {
    if approval_key(reason).is_some() || mcp_grant_entry(reason).is_some() {
        return true;
    }
    COMMAND_CONFIRM_SUFFIXES
        .iter()
        .chain(UNGRANTABLE_COMMAND_SUFFIXES)
        .any(|suffix| reason.strip_suffix(suffix).is_some())
}

/// What a stopped task actually needs, if anything.
///
/// `None` means the task is not stuck on a permission at all (still running,
/// or stopped for a reason no `agent resume` flag could ever address, such
/// as an exhausted budget).
///
/// Both `InputRequired` and grant-stuck `Interrupted` tasks qualify: since
/// deny-and-continue, a refusal no longer stops the turn, so a task that
/// worked around refusals until it could not proceed lands `Interrupted`
/// with the last refusal hint as its `stop_reason` (see
/// `AgentRuntime::finish`) instead of `InputRequired`. Any other status, or
/// an `Interrupted` task whose reason names no grant, has no need.
pub(crate) fn blocked_need(task: &AgentTask) -> Option<BlockedNeed> {
    use dsh_types::agent::TaskStatus;

    // Reconciliation always comes first: `run_task` refuses to proceed past
    // an unresolved `pending_operation` no matter what other grants a resume
    // supplies, so suggesting anything else first would be a dead end.
    if task.pending_operation.is_some() {
        return Some(BlockedNeed {
            what: "a previous operation's outcome was never confirmed".to_string(),
            fix: Some(format!(
                "agent resume {} --reconcile 'describe what you actually observed'",
                task.id
            )),
        });
    }

    let grant_stuck = match task.status {
        TaskStatus::InputRequired => true,
        TaskStatus::Interrupted => task
            .stop_reason
            .as_deref()
            .is_some_and(is_grant_hint),
        _ => false,
    };
    if !grant_stuck {
        return None;
    }

    let reason = task.stop_reason.as_deref().unwrap_or("");

    if let Some(key) = approval_key(reason) {
        return Some(from_approval_key(&task.id, reason, key));
    }

    if let Some(entry) = mcp_grant_entry(reason) {
        return Some(BlockedNeed {
            what: reason.to_string(),
            fix: Some(format!("agent resume {} --allow-mcp '{entry}'", task.id)),
        });
    }

    for suffix in COMMAND_CONFIRM_SUFFIXES {
        if let Some(command) = reason.strip_suffix(suffix) {
            return Some(BlockedNeed {
                what: format!("permission to run `{command}`"),
                fix: Some(format!(
                    "agent resume {} --allow-command '{command}'",
                    task.id
                )),
            });
        }
    }

    // Anything else - including `UNGRANTABLE_COMMAND_SUFFIXES`, refused in a
    // command shape but satisfiable by no flag - reports the reason with no
    // fix, rather than a resume command that would fail on the spot.

    Some(BlockedNeed {
        what: reason.to_string(),
        fix: None,
    })
}

/// Pulls `X` out of a `stop_reason` ending in `[approval_key: X]`, the shape
/// `confirm_agent_action_with_preview` records via `AgentRuntime::note_denial`.
pub(crate) fn approval_key(reason: &str) -> Option<&str> {
    let start = reason.rfind("[approval_key: ")? + "[approval_key: ".len();
    let end = reason[start..].find(']')?;
    Some(&reason[start..start + end])
}

/// Pulls the `mcp:tool:args` grant entry out of an MCP call's `stop_reason`
/// (see [`MCP_GRANT_REASON_PREFIX`]), which has no `[approval_key: ...]`
/// marker to use [`approval_key`] on instead.
///
/// The trailing `)` is required, not optional: a hint truncated mid-entry
/// (see `AgentRuntime::note_denial`'s length cap) must resolve to no fix -
/// falling back to the truncated text would suggest an `--allow-mcp` key
/// that can never byte-match. Full-length reasons always end with it.
pub(crate) fn mcp_grant_entry(reason: &str) -> Option<&str> {
    let start = reason.find(MCP_GRANT_REASON_PREFIX)? + MCP_GRANT_REASON_PREFIX.len();
    let rest = &reason[start..];
    rest.strip_suffix(')')
}

/// The classified form of an `[approval_key: ...]` marker, shared by
/// [`blocked_need`] (display) and `agent approve` (grant application) so the
/// two can never disagree about what a key means.
pub(crate) enum ApprovalKeyKind {
    /// A file write: the parent directory to grant with `--write`.
    WriteDir(String),
    /// An MCP call: the exact entry to grant with `--allow-mcp`.
    Mcp(String),
    /// A cron change: the verb and job it spells (`cron <verb> <job>`).
    Cron(String, String),
    /// Anything no flag can satisfy (`hook:`, `sensitive:`, `delete:`, ...).
    Other,
}

pub(crate) fn classify_approval_key(key: &str) -> ApprovalKeyKind {
    if let Some(path) = key.strip_prefix("write:") {
        let dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.to_string());
        return ApprovalKeyKind::WriteDir(dir);
    }
    if key.starts_with("mcp:") {
        return ApprovalKeyKind::Mcp(key.to_string());
    }
    if let Some(rest) = key.strip_prefix("cron:") {
        let (action, job) = rest.split_once(':').unwrap_or((rest, ""));
        return ApprovalKeyKind::Cron(action.to_string(), job.to_string());
    }
    ApprovalKeyKind::Other
}

/// Whether a cron key spells a runnable `cron <verb> <job>` (see below).
pub(crate) fn is_runnable_cron_key(action: &str) -> bool {
    matches!(action, "pause" | "resume" | "remove" | "run")
}

fn from_approval_key(task_id: &str, reason: &str, key: &str) -> BlockedNeed {
    let what = reason
        .split(" [approval_key:")
        .next()
        .unwrap_or(reason)
        .to_string();

    match classify_approval_key(key) {
        ApprovalKeyKind::WriteDir(dir) => BlockedNeed {
            what,
            fix: Some(format!("agent resume {task_id} --write {dir}")),
        },
        ApprovalKeyKind::Mcp(entry) => BlockedNeed {
            what,
            fix: Some(format!("agent resume {task_id} --allow-mcp '{entry}'")),
        },
        ApprovalKeyKind::Cron(action, job) => {
            // Only these four `CronToolAction`s (`dsh-types/src/cron/tool.rs`)
            // map onto a bare `cron <verb> <job>` exactly as spelled here.
            // `create`/`update` are not even reachable this way - `cron create`
            // needs a schedule and command/`--agent` flags this key does not
            // carry, and there is no `cron update` subcommand at all (the real
            // verb is `cron edit`, `dsh/src/cron/mod.rs`) - and `ack` is invoked
            // as `cron incidents ack <id>`, not `cron ack <id>`, with an incident
            // id this key does not carry either (`CronToolAction::Ack` has no
            // `job`/`name`, so `job` here is the literal placeholder `"?"`).
            // Offering any of those as a "verbatim" command would fail the
            // instant a person ran it, which is worse than naming no fix at all.
            let fix = is_runnable_cron_key(&action)
                .then(|| format!("cron {action} {job} && agent resume {task_id}"));
            BlockedNeed { what, fix }
        }
        // `hook:`, `sensitive:`, `delete:` - none of `agent resume`'s flags can
        // satisfy these. Saying so plainly beats suggesting a flag that would
        // silently fail to help: `docs/agent.md` already documents that skill
        // deletion and a hook's own `ask` cannot be granted this way.
        ApprovalKeyKind::Other => BlockedNeed { what, fix: None },
    }
}

#[cfg(test)]
mod tests;
