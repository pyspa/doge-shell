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

use dsh_types::agent::AgentTask;

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
/// Anything not in this list came from a place this module does not (yet)
/// know how to resolve into an `--allow-command`.
const COMMAND_CONFIRM_SUFFIXES: &[&str] = &[
    ": command is not in the task's exact command grants",
    ": running a skill script needs its own approval",
];

/// The text `evaluate_agent_tool` (`dsh/src/proxy/agent_policy.rs`) gives for
/// an MCP call outside the task's grant, exactly as `authorize_mcp_tool`
/// (`dsh-builtin/src/chatgpt/tool/mod.rs`) wraps it into `stop_reason`:
/// `"AI wants to call MCP tool: `NAME` ({this}{entry})"`. `entry` is already
/// the exact `mcp:tool:args` string `--allow-mcp` expects
/// (`SafetyGuard::mcp_allowlist_entry`), so it is pulled out of the plain
/// message text rather than an `[approval_key: ...]` marker - see this
/// module's doc comment for why MCP has no marker to match on instead.
const MCP_GRANT_REASON_PREFIX: &str = "external operation needs an exact task grant: ";

/// What a task at rest actually needs, if anything.
///
/// `None` means the task is not stuck on a permission at all (still running,
/// or stopped for a reason no `agent resume` flag could ever address, such
/// as an exhausted budget).
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

    if task.status != TaskStatus::InputRequired {
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

    Some(BlockedNeed {
        what: reason.to_string(),
        fix: None,
    })
}

/// Pulls `X` out of a `stop_reason` ending in `[approval_key: X]`, the shape
/// `confirm_agent_action_with_preview` writes.
fn approval_key(reason: &str) -> Option<&str> {
    let start = reason.rfind("[approval_key: ")? + "[approval_key: ".len();
    let end = reason[start..].find(']')?;
    Some(&reason[start..start + end])
}

/// Pulls the `mcp:tool:args` grant entry out of an MCP call's `stop_reason`
/// (see [`MCP_GRANT_REASON_PREFIX`]), which has no `[approval_key: ...]`
/// marker to use [`approval_key`] on instead.
fn mcp_grant_entry(reason: &str) -> Option<&str> {
    let start = reason.find(MCP_GRANT_REASON_PREFIX)? + MCP_GRANT_REASON_PREFIX.len();
    let rest = &reason[start..];
    Some(rest.strip_suffix(')').unwrap_or(rest))
}

fn from_approval_key(task_id: &str, reason: &str, key: &str) -> BlockedNeed {
    let what = reason
        .split(" [approval_key:")
        .next()
        .unwrap_or(reason)
        .to_string();

    if let Some(path) = key.strip_prefix("write:") {
        let dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.to_string());
        return BlockedNeed {
            what,
            fix: Some(format!("agent resume {task_id} --write {dir}")),
        };
    }
    if key.starts_with("mcp:") {
        return BlockedNeed {
            what,
            fix: Some(format!("agent resume {task_id} --allow-mcp '{key}'")),
        };
    }
    if let Some(rest) = key.strip_prefix("cron:") {
        let (action, job) = rest.split_once(':').unwrap_or((rest, ""));
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
        let fix = matches!(action, "pause" | "resume" | "remove" | "run")
            .then(|| format!("cron {action} {job} && agent resume {task_id}"));
        return BlockedNeed { what, fix };
    }
    // `hook:`, `sensitive:`, `delete:` - none of `agent resume`'s flags can
    // satisfy these. Saying so plainly beats suggesting a flag that would
    // silently fail to help: `docs/agent.md` already documents that skill
    // deletion and a hook's own `ask` cannot be granted this way.
    BlockedNeed { what, fix: None }
}

#[cfg(test)]
mod tests;
