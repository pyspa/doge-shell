//! `agent doctor`: what a person cannot see from `agent list` alone -
//! detached tasks a dead process left behind, unresolved approvals piling
//! up, and state-directory files nothing else in `agent` reports on its own.
//!
//! Kept apart from `dsh-builtin/src/doctor/ai.rs` for the same reason `cron
//! doctor` is split from `dsh-builtin`'s `doctor`: this needs
//! `SqliteTaskStore`, which lives in `dsh`, not `dsh-builtin`.

use super::{SqliteTaskStore, blocked, cli};
use anyhow::{Result, bail};
use dsh_builtin::shell_capabilities::AgentTaskStore as _;
use dsh_types::Context;
use dsh_types::agent::TaskStatus;
use serde_json::{Value, json};
use std::collections::HashSet;

/// How long an `InputRequired` task must sit before it is worth a warning -
/// short-lived ones are just the normal shape of an interactive `agent run`.
const STALE_APPROVAL_SECS: i64 = 3600;

pub(crate) struct DoctorReport {
    lines: Vec<String>,
    ok: usize,
    warn: usize,
}

impl DoctorReport {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            ok: 0,
            warn: 0,
        }
    }
    fn ok_line(&mut self, text: impl Into<String>) {
        self.lines.push(format!("ok {}", text.into()));
        self.ok += 1;
    }
    fn warn_line(&mut self, text: impl Into<String>) {
        self.lines.push(format!("warn {}", text.into()));
        self.warn += 1;
    }
    fn to_json(&self) -> Value {
        json!({"ok": self.ok, "warn": self.warn, "lines": self.lines})
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(crate) fn build_report(
    shell: &mut crate::shell::Shell,
    store: &SqliteTaskStore,
) -> Result<DoctorReport> {
    let mut report = DoctorReport::new();
    let tasks = store.list()?;
    let now = now();

    if tasks.is_empty() {
        report.ok_line("no tasks recorded");
    } else {
        let oldest = tasks
            .iter()
            .map(|task| task.created_at)
            .min()
            .unwrap_or(now);
        report.ok_line(format!(
            "{} task(s) recorded, oldest {}",
            tasks.len(),
            cli::format_age(now, oldest)
        ));
    }

    for task in &tasks {
        // `task.stop_reason` already carries this - set to the same fixed
        // text by `recover_interrupted` (`dsh/src/agent.rs`) whenever it
        // marks a task `Interrupted` this way - so this needs no separate
        // `store.events()` round trip per task just to inspect the last
        // event's kind.
        if task.status == TaskStatus::Interrupted
            && task.stop_reason.as_deref() == Some(super::RECOVERED_STOP_REASON)
        {
            report.warn_line(format!(
                "crashed {}: a previous process holding this task ended without finishing it; `agent resume {}` to continue",
                task.id, task.id
            ));
        }
        if let Some(need) = blocked::blocked_need(task) {
            let age = cli::format_age(now, task.created_at);
            let fix = need
                .fix
                .unwrap_or_else(|| format!("agent show {}", task.id));
            if task.pending_operation.is_some() {
                report.warn_line(format!("needs-reconcile {} ({age}): {fix}", task.id));
            } else if now - task.created_at > STALE_APPROVAL_SECS {
                report.warn_line(format!("needs-approval {} ({age}): {fix}", task.id));
            }
        }
    }

    let known: HashSet<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
    let mut stale_artifacts = 0usize;
    // `store.root`, not `config_paths::agent_state_dir()`: this must be the
    // same directory `store` itself resolves artifacts under, which a test
    // opens somewhere other than the real state directory.
    if let Ok(entries) = std::fs::read_dir(&store.root) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == "locks" || uuid::Uuid::parse_str(name).is_err() {
                continue;
            }
            if !known.contains(name) {
                stale_artifacts += 1;
            }
        }
    }
    if stale_artifacts > 0 {
        report.warn_line(format!(
            "stale-artifacts {stale_artifacts}: director{} under the agent state directory with no matching task row; inspect and remove by hand",
            if stale_artifacts == 1 { "y" } else { "ies" }
        ));
    }

    if super::setting(shell, "AI_AGENT_TOKEN_BUDGET").is_none()
        && super::setting(shell, "AI_AGENT_TIMEOUT_SECS").is_none()
    {
        report.warn_line(
            "no-default-budget: AI_AGENT_TOKEN_BUDGET/AI_AGENT_TIMEOUT_SECS are both unset; every `agent run`/`--detach` needs --tokens/--timeout spelled out explicitly",
        );
    }

    Ok(report)
}

pub(crate) fn run(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &SqliteTaskStore,
    args: &[String],
) -> Result<()> {
    let json_output = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        [other, ..] => bail!("unsupported option {other}"),
    };
    let report = build_report(shell, store)?;
    if json_output {
        ctx.write_stdout(&serde_json::to_string_pretty(&report.to_json())?)?;
    } else {
        for line in &report.lines {
            ctx.write_stdout(line)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
