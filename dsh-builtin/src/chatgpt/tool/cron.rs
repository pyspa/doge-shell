//! Letting the agent manage its own cron jobs.
//!
//! `cron` itself (`dsh/src/cron/`) is a builtin, not a chat tool - `execute`
//! runs everything under `sh -c` (`tool/execute/capture.rs`), and a builtin
//! never reaches a shell it is not invoked through. Without this, an agent
//! that read the `dsh-cron` skill had a complete plan and no way to carry it
//! out. Argument parsing, schedule validation, grant canonicalisation and the
//! store all stay exactly where they are (`dsh/src/cron/cli/parse.rs`,
//! `dsh-builtin/src/agent/grant.rs`, `CronStore`): this tool only turns one
//! JSON call into the same argv `cron add`/`cron edit` already validate, via
//! [`crate::shell_capabilities::CronToolHost`].
//!
//! No `notepad` action: a job's notepad directory is already in its own
//! `read`/`write` grant (`dsh/src/cron/run_job.rs::notepad_grant`), so the
//! model reads and writes it with the `read_file`/`edit` tools it already has.
//!
//! # Safety, layered
//!
//! - A brand-new job (`create`) is always registered paused, regardless of
//!   what was asked for - a person must `cron resume` it before it ever
//!   fires. This alone would be enough even with nothing else below.
//! - Every write action (`create`/`update`/`pause`/`resume`/`remove`/`run`/
//!   `ack`) asks the user first, through the same `confirm_agent_action` every
//!   other write tool uses. Under an unattended task that always means a
//!   recorded refusal the turn works around - the same deny-and-continue
//!   `edit`/`execute` already produce there, not a new kind of stop.
//! - A cron job's grant can never exceed the calling task's own: widening one
//!   (`--read`/`--write`/`--allow-command`/`--allow-mcp`/`--network`/`--env`/
//!   `--sandbox`) past what the task itself was granted is refused outright,
//!   before anyone is asked anything. Outside a task (the `!` chat has no
//!   grant of its own to exceed) the full grant is simply named in the
//!   question a person is asked.

use crate::shell_capabilities::ChatToolHost;
use dsh_types::agent::TaskGrant;
use dsh_types::cron::tool::{CronToolAction, CronToolRequest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) const NAME: &str = "cron_manage";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Add, edit, run or inspect a doge-shell cron job - a persistent scheduled shell command or unattended agent task, surviving restarts and independent of this conversation. `list`/`show`/`history`/`logs`/`incidents`/`status`/`doctor` read without asking; `create`/`update`/`pause`/`resume`/`remove`/`run`/`ack` always ask first. A job this tool creates always starts paused - a person resumes it after checking `cron run` once. `run` marks a job due for the next tick (session runner or external `cron tick`, usually within about a minute); it does not run synchronously and does not accept a one-off prompt, and it refuses a job that is still paused or blocked by an open incident (`resume` or `ack` it first). `logs` returns one run's full recorded stdout/stderr (an agent job's own summary of what it did, for an agent job) - use it, not `history`'s one-line preview, to actually read what a past run produced; refused if the job's own directory falls outside this task's grant. An agent job's goal starts a brand-new, self-contained session with no memory of this conversation and cannot ask a question - write it as a complete instruction. Prefer `update` on an existing job over creating a near-duplicate; always `list` first rather than guessing a job's name.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "show", "history", "logs", "incidents", "status", "doctor", "create", "update", "pause", "resume", "remove", "run", "ack"],
                        "description": "`create` needs `schedule` and (`command` or, with `agent`, `goal`). `show`/`update`/`pause`/`resume`/`remove`/`run` need `job`. `logs` needs `job` or `run`. `ack` needs `incident_id` (from `incidents`)."
                    },
                    "job": {
                        "type": "string",
                        "description": "A job's name or id, from `list`. `logs` reads that job's most recently finished run when `run` is not given."
                    },
                    "run": {
                        "type": "string",
                        "description": "`logs` only: a specific run id (or a unique prefix of one, from `history`), instead of `job`'s latest finished run."
                    },
                    "name": {
                        "type": "string",
                        "description": "`create`/`update`: reference name. `create` refuses a name already in use unless `force` is set."
                    },
                    "schedule": {
                        "type": "string",
                        "description": "`create`'s required schedule, or `update`'s new one. An interval (`30s`/`5m`/`1h`, 5s-24h), a 5-field cron expression (`0 9 * * mon-fri`), or a macro (`@daily`, `@hourly`, ...)."
                    },
                    "command": {
                        "type": "string",
                        "description": "A shell job's full command line, run under `sh -c` from `cwd` - no aliases, abbreviations or dogesh builtins. Required for `create` unless `agent` is set; `update`'s equivalent of the shell command."
                    },
                    "agent": {
                        "type": "boolean",
                        "description": "`create` only: register an unattended agent job instead of a shell job. Needs `goal`, `tokens`, and at least one of `read`/`write`."
                    },
                    "goal": {
                        "type": "string",
                        "description": "An agent job's instruction (`create` with `agent`, or `update`). Runs as a brand-new session with no memory of this conversation and no way to ask a question - it must be fully self-contained."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory. Defaults to the directory this call runs from."
                    },
                    "tokens": {
                        "type": "integer",
                        "description": "An agent job's per-run token ceiling. Required when `agent` is set on `create`."
                    },
                    "max_tokens_per_day": {
                        "type": "integer",
                        "description": "An agent job's rolling 24-hour token ceiling, across every run - without it a frequent schedule has no real limit."
                    },
                    "check": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "An agent job's completion criteria. Verified against recorded tool results, so word them as concrete, checkable evidence, not a subjective judgement."
                    },
                    "on": {
                        "type": "string",
                        "enum": ["never", "failure", "change", "both", "always"],
                        "description": "When a run is worth flagging (default `both`). Recorded but not yet delivered anywhere as a notification - read a run's outcome with `history`, not this."
                    },
                    "timeout": {
                        "type": "string",
                        "description": "Wall-clock limit per run, e.g. `60` or `5m` (default 60s). For an agent job this is also its cooperative time budget."
                    },
                    "catchup": {
                        "type": "string",
                        "description": "How long a missed run may still be worth doing before the backlog collapses to one (default 1h)."
                    },
                    "read": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "An agent job's readable directories. Under a task, every entry must already be covered by this task's own grant."
                    },
                    "write": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "An agent job's writable directories. Under a task, every entry must already be covered by this task's own grant."
                    },
                    "allow_command": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Exact commands the agent job may run unattended. Under a task, every entry must already be one of this task's own."
                    },
                    "allow_mcp": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Exact MCP approval keys (as `agent show` prints them - copy one, do not guess it). Under a task, every entry must already be one of this task's own."
                    },
                    "network": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Exact hosts the agent job may reach. Under a task, every entry must already be one of this task's own."
                    },
                    "env": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Environment variable NAMEs the agent job may read (the value comes from the shell's own environment at run time, never from this call). Under a task, every entry must already be one of this task's own."
                    },
                    "sandbox": {
                        "type": "boolean",
                        "description": "Run the agent job's commands through the sandbox runtime. Under a task, only if this task's own grant already has it."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "`create` only: replace an existing job of the same name instead of refusing."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "`history`: how many finished runs to return (default 20)."
                    },
                    "failed": {
                        "type": "boolean",
                        "description": "`history`: only failed runs."
                    },
                    "incident_id": {
                        "type": "integer",
                        "description": "`ack`'s target, from `incidents`."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }
        }
    })
}

fn opt_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|field| match field {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

fn str_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn bool_flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn parse_request(arguments: &str) -> Result<CronToolRequest, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for {NAME} tool: {err}"))?;

    let action = parsed
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("chat: {NAME} requires `action`"))?;
    let action = CronToolAction::parse(action).map_err(|err| format!("chat: {err}"))?;

    Ok(CronToolRequest {
        action: Some(action),
        job: opt_str(&parsed, "job"),
        name: opt_str(&parsed, "name"),
        schedule: opt_str(&parsed, "schedule"),
        command: opt_str(&parsed, "command"),
        goal: opt_str(&parsed, "goal"),
        cwd: opt_str(&parsed, "cwd"),
        agent: bool_flag(&parsed, "agent"),
        tokens: opt_str(&parsed, "tokens"),
        max_tokens_per_day: opt_str(&parsed, "max_tokens_per_day"),
        check: str_list(&parsed, "check"),
        on: opt_str(&parsed, "on"),
        timeout: opt_str(&parsed, "timeout"),
        catchup: opt_str(&parsed, "catchup"),
        read: str_list(&parsed, "read"),
        write: str_list(&parsed, "write"),
        allow_command: str_list(&parsed, "allow_command"),
        allow_mcp: str_list(&parsed, "allow_mcp"),
        network: str_list(&parsed, "network"),
        env: str_list(&parsed, "env"),
        sandbox: bool_flag(&parsed, "sandbox"),
        force: bool_flag(&parsed, "force"),
        limit: opt_str(&parsed, "limit"),
        failed: bool_flag(&parsed, "failed"),
        incident_id: opt_str(&parsed, "incident_id"),
        run: opt_str(&parsed, "run"),
    })
}

/// A path this call named, resolved the same way `apply_grant_option` does.
///
/// `None` when it does not resolve (does not exist, most likely) - that is
/// not this check's business to report; `apply_grant_option` will refuse it
/// with a much clearer "no such directory" once the call actually reaches it,
/// so a path that fails to resolve is let through here rather than reported
/// as an ungranted one.
fn resolved(candidate: &str) -> Option<PathBuf> {
    Path::new(shellexpand::tilde(candidate).as_ref())
        .canonicalize()
        .ok()
}

fn path_within(candidate: &str, roots: &[PathBuf]) -> bool {
    match resolved(candidate) {
        Some(path) => roots.iter().any(|root| path.starts_with(root)),
        None => true,
    }
}

/// The first grant-shaped field in `request` that reaches past `task_grant`,
/// if any. `None` means every grant-shaped field named (there may be none)
/// already fits inside what the calling task itself was granted.
fn grant_exceeds_task(request: &CronToolRequest, task_grant: &TaskGrant) -> Option<String> {
    for path in &request.read {
        // Readable is the weaker claim - anywhere the task can write, it can
        // also read - so either root list satisfies a `--read` ask.
        if !path_within(path, &task_grant.read_roots) && !path_within(path, &task_grant.write_roots)
        {
            return Some(format!(
                "`read: {path}` is outside this task's own read/write grant"
            ));
        }
    }
    for path in &request.write {
        if !path_within(path, &task_grant.write_roots) {
            return Some(format!(
                "`write: {path}` is outside this task's own write grant"
            ));
        }
    }
    for command in &request.allow_command {
        if !task_grant.commands.iter().any(|granted| granted == command) {
            return Some(format!(
                "`allow_command: {command}` is not one of this task's own granted commands"
            ));
        }
    }
    for entry in &request.allow_mcp {
        if !task_grant.mcp_calls.iter().any(|granted| granted == entry) {
            return Some(format!(
                "`allow_mcp: {entry}` is not one of this task's own granted MCP calls"
            ));
        }
    }
    for host in &request.network {
        if !task_grant
            .network_hosts
            .iter()
            .any(|granted| granted == host)
        {
            return Some(format!(
                "`network: {host}` is not one of this task's own granted hosts"
            ));
        }
    }
    for name in &request.env {
        if !task_grant.environment.iter().any(|granted| granted == name) {
            return Some(format!(
                "`env: {name}` is not one of this task's own granted variables"
            ));
        }
    }
    if request.sandbox && !task_grant.sandbox {
        return Some("`sandbox` is not part of this task's own grant".to_string());
    }
    None
}

/// Whether `request` names any grant-shaped field at all, for the interactive
/// confirmation text (there is nothing to exceed outside a task, but a person
/// answering the question still deserves to see it).
fn grant_summary(request: &CronToolRequest) -> Option<String> {
    if request.read.is_empty()
        && request.write.is_empty()
        && request.allow_command.is_empty()
        && request.allow_mcp.is_empty()
        && request.network.is_empty()
        && request.env.is_empty()
        && !request.sandbox
    {
        return None;
    }
    let mut parts = Vec::new();
    if !request.read.is_empty() {
        parts.push(format!("read={:?}", request.read));
    }
    if !request.write.is_empty() {
        parts.push(format!("write={:?}", request.write));
    }
    if !request.allow_command.is_empty() {
        parts.push(format!("allow_command={:?}", request.allow_command));
    }
    if !request.allow_mcp.is_empty() {
        parts.push(format!("allow_mcp={:?}", request.allow_mcp));
    }
    if !request.network.is_empty() {
        parts.push(format!("network={:?}", request.network));
    }
    if !request.env.is_empty() {
        parts.push(format!("env={:?}", request.env));
    }
    if request.sandbox {
        parts.push("sandbox=true".to_string());
    }
    Some(parts.join(" "))
}

/// Every field `update` can change, rendered as `name=value` pairs - so the
/// person answering the confirmation sees exactly what is about to change,
/// the same way `grant_summary` already does for the grant-shaped fields.
fn update_summary(request: &CronToolRequest) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(name) = &request.name {
        parts.push(format!("name={name}"));
    }
    if let Some(schedule) = &request.schedule {
        parts.push(format!("schedule={schedule}"));
    }
    if let Some(command) = &request.command {
        parts.push(format!("command=`{command}`"));
    }
    if let Some(goal) = &request.goal {
        parts.push(format!("goal=`{goal}`"));
    }
    if let Some(cwd) = &request.cwd {
        parts.push(format!("cwd={cwd}"));
    }
    if let Some(on) = &request.on {
        parts.push(format!("on={on}"));
    }
    if let Some(timeout) = &request.timeout {
        parts.push(format!("timeout={timeout}"));
    }
    if let Some(catchup) = &request.catchup {
        parts.push(format!("catchup={catchup}"));
    }
    if let Some(tokens) = &request.tokens {
        parts.push(format!("tokens={tokens}"));
    }
    if let Some(ceiling) = &request.max_tokens_per_day {
        parts.push(format!("max_tokens_per_day={ceiling}"));
    }
    if !request.check.is_empty() {
        parts.push(format!("check={:?}", request.check));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(", "))
}

fn confirm_message(action: CronToolAction, request: &CronToolRequest) -> String {
    let job = request.job.as_deref().unwrap_or("?");
    let mut message = match action {
        CronToolAction::Create => format!(
            "AI wants to create a {} cron job{} on `{}`: `{}` (registered paused)",
            if request.agent { "agent" } else { "shell" },
            request
                .name
                .as_deref()
                .map(|name| format!(" named `{name}`"))
                .unwrap_or_default(),
            request.schedule.as_deref().unwrap_or("?"),
            if request.agent {
                request.goal.as_deref()
            } else {
                request.command.as_deref()
            }
            .unwrap_or("?"),
        ),
        CronToolAction::Update => match update_summary(request) {
            Some(summary) => format!("AI wants to change cron job `{job}`: {summary}"),
            None => format!("AI wants to change cron job `{job}`"),
        },
        CronToolAction::Pause => format!("AI wants to pause cron job `{job}`"),
        CronToolAction::Resume => format!("AI wants to resume cron job `{job}`"),
        CronToolAction::Remove => format!("AI wants to remove cron job `{job}`"),
        CronToolAction::Run => format!("AI wants to mark cron job `{job}` due for its next run"),
        CronToolAction::Ack => format!(
            "AI wants to acknowledge cron incident {}",
            request.incident_id.as_deref().unwrap_or("?")
        ),
        // Every other action is read-only and never reaches this function.
        _ => format!("AI wants to change cron job `{job}`"),
    };
    if let Some(grant) = grant_summary(request) {
        message.push_str(&format!(" (grant: {grant})"));
    }
    message
}

fn approval_key(action: CronToolAction, request: &CronToolRequest) -> String {
    let job = request
        .job
        .as_deref()
        .or(request.name.as_deref())
        .unwrap_or("?");
    format!("cron:{action}:{job}")
}

/// Everything that can be checked before anyone is asked anything - the same
/// ordering `skill_manage`'s own `validate` follows, and for the same reason:
/// a question about a call that was always going to fail trains people to
/// answer without reading.
fn validate_request(action: CronToolAction, request: &CronToolRequest) -> Result<(), String> {
    let need = |field: &Option<String>, name: &str| -> Result<(), String> {
        if field.is_none() {
            return Err(format!("chat: {NAME} `{action}` requires `{name}`"));
        }
        Ok(())
    };
    match action {
        CronToolAction::Create => {
            need(&request.schedule, "schedule")?;
            if request.agent {
                need(&request.goal, "goal")
            } else {
                need(&request.command, "command")
            }
        }
        CronToolAction::Show
        | CronToolAction::Update
        | CronToolAction::Pause
        | CronToolAction::Resume
        | CronToolAction::Remove
        | CronToolAction::Run => need(&request.job, "job"),
        // Unlike every other job-selector action, `logs` accepts a bare
        // `run` id in place of `job` - the same as `cron logs --run <id>`
        // needing no job name either, once the run itself pins it down.
        CronToolAction::Logs => {
            if request.job.is_none() && request.run.is_none() {
                return Err(format!("chat: {NAME} `logs` requires `job` or `run`"));
            }
            Ok(())
        }
        CronToolAction::Ack => need(&request.incident_id, "incident_id"),
        CronToolAction::List
        | CronToolAction::History
        | CronToolAction::Incidents
        | CronToolAction::Status
        | CronToolAction::Doctor => Ok(()),
    }
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    let request = parse_request(arguments)?;
    let action = request
        .action
        .expect("parse_request always sets action or returns early");
    validate_request(action, &request)?;

    if action.is_write() {
        if let Some(runtime) = proxy.agent_runtime() {
            let task_grant = runtime.lock().task.grant.clone();
            if let Some(reason) = grant_exceeds_task(&request, &task_grant) {
                return Err(format!(
                    "chat: {NAME} refused: {reason}. A cron job's grant can never exceed this \
                     task's own; ask a person to widen it with `cron edit` instead."
                ));
            }
        }
        if !super::confirm_agent_action(
            proxy,
            &approval_key(action, &request),
            &confirm_message(action, &request),
        )? {
            return Ok(format!("cron_manage: {action} cancelled by user."));
        }
    }

    proxy
        .cron_tool_call(&request)
        .map(|value| value.to_string())
        .map_err(|error| format!("chat: {error}"))
}

#[cfg(test)]
mod tests;
