//! Letting the agent manage its own cron jobs.
//!
//! `cron` itself (`dsh/src/cron/`) is a builtin, not a chat tool - `execute`
//! runs everything under `sh -c` (`tool/execute/capture.rs`), and a builtin
//! never reaches a shell it is not invoked through. Without this, an agent
//! that read the `dsh-cron` skill had a complete plan and no way to carry it
//! out. Argument parsing, schedule validation and the store all stay
//! exactly where they are (`dsh/src/cron/cli/parse.rs`, `CronStore`): this
//! tool only turns one JSON call into the same argv `cron add`/`cron edit`
//! already validate, via [`crate::shell_capabilities::CronToolHost`].
//!
//! No `notepad` action: the model reads a job's notepad with the
//! `read_file` tool it already has.
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

use crate::shell_capabilities::ChatToolHost;
use dsh_types::cron::tool::{CronToolAction, CronToolRequest};
use serde_json::{Value, json};

pub(crate) const NAME: &str = "cron_manage";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Add, edit, run or inspect a doge-shell cron job - a persistent scheduled shell command, surviving restarts and independent of this conversation. `list`/`show`/`history`/`logs`/`incidents`/`status`/`doctor` read without asking; `create`/`update`/`pause`/`resume`/`remove`/`run`/`ack` always ask first. A job this tool creates always starts paused - a person resumes it after checking `cron run` once. `run` marks a job due for the next tick (session runner or external `cron tick`, usually within about a minute); it does not run synchronously and does not accept a one-off prompt, and it refuses a job that is still paused or blocked by an open incident (`resume` or `ack` it first). `logs` returns one run's full recorded stdout/stderr - use it, not `history`'s one-line preview, to actually read what a past run produced; refused if the job's own directory falls outside this task's grant. Prefer `update` on an existing job over creating a near-duplicate; always `list` first rather than guessing a job's name.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "show", "history", "logs", "incidents", "status", "doctor", "create", "update", "pause", "resume", "remove", "run", "ack"],
                        "description": "`create` needs `schedule` and `command`. `show`/`update`/`pause`/`resume`/`remove`/`run` need `job`. `logs` needs `job` or `run`. `ack` needs `incident_id` (from `incidents`)."
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
                        "description": "A shell job's full command line, run under `sh -c` from `cwd` - no aliases, abbreviations or dogesh builtins. Required for `create`; `update`'s equivalent of the shell command."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory. Defaults to the directory this call runs from."
                    },
                    "on": {
                        "type": "string",
                        "enum": ["never", "failure", "change", "both", "always"],
                        "description": "When a run is worth flagging (default `both`). Recorded but not yet delivered anywhere as a notification - read a run's outcome with `history`, not this."
                    },
                    "timeout": {
                        "type": "string",
                        "description": "Wall-clock limit per run, e.g. `60` or `5m` (default 60s)."
                    },
                    "catchup": {
                        "type": "string",
                        "description": "How long a missed run may still be worth doing before the backlog collapses to one (default 1h)."
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

/// Every field `update` can change, rendered as `name=value` pairs - so the
/// person answering the confirmation sees exactly what is about to change.
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
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(", "))
}

fn confirm_message(action: CronToolAction, request: &CronToolRequest) -> String {
    let job = request.job.as_deref().unwrap_or("?");
    match action {
        CronToolAction::Create => format!(
            "AI wants to create a shell cron job{} on `{}`: `{}` (registered paused)",
            request
                .name
                .as_deref()
                .map(|name| format!(" named `{name}`"))
                .unwrap_or_default(),
            request.schedule.as_deref().unwrap_or("?"),
            request.command.as_deref().unwrap_or("?"),
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
    }
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
    if request.agent
        || request.goal.is_some()
        || !request.check.is_empty()
        || request.max_tokens_per_day.is_some()
        || !request.read.is_empty()
        || !request.write.is_empty()
        || !request.allow_command.is_empty()
        || !request.allow_mcp.is_empty()
        || !request.network.is_empty()
        || !request.env.is_empty()
        || request.sandbox
    {
        return Err(format!("chat: {NAME} agent jobs are no longer supported"));
    }
    match action {
        CronToolAction::Create => {
            need(&request.schedule, "schedule")?;
            need(&request.command, "command")?;
            Ok(())
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

    if action.is_write()
        && !super::confirm_agent_action(
            proxy,
            &approval_key(action, &request),
            &confirm_message(action, &request),
        )?
    {
        return Ok(format!("cron_manage: {action} cancelled by user."));
    }

    proxy
        .cron_tool_call(&request)
        .map(|value| value.to_string())
        .map_err(|error| format!("chat: {error}"))
}

#[cfg(test)]
mod tests;
