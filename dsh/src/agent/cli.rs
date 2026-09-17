//! Parsing and rendering for `agent list`/`agent logs`/`agent wait` - the
//! commands a person uses to watch a task without blocking on it, which
//! matters once `--detach` (`dsh/src/agent/detach.rs`) means a task can be
//! running when nobody is looking at it.

use super::{SqliteTaskStore, locks, summary};
use anyhow::{Context as _, Result, bail};
use dsh_builtin::shell_capabilities::AgentTaskStore as _;
use dsh_types::Context;
use dsh_types::agent::{AgentTask, TaskEvent, TaskStatus};
use serde_json::{Value, json};
use std::time::Duration;

/// Hides a finished task from the default `agent list` view once it is this
/// old, so a long-lived shell does not accumulate an ever-growing list of
/// things nobody needs to see again. `--all` bypasses it entirely.
const HIDE_FINISHED_AFTER_SECS: i64 = 24 * 3600;
/// How often `logs --follow`/`wait` re-check the store.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(crate) fn format_age(now: i64, created_at: i64) -> String {
    let secs = (now - created_at).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// A task's goal, flattened to one line and clamped, for a table row.
fn goal_preview(goal: &str) -> String {
    dsh_types::text::clamp_chars(&goal.replace('\n', " "), 60)
}

/// Whether some process is still actively working on `task` - not just
/// whether its stored status says `Running`. A `--detach` child can hold the
/// lock briefly before `run_task` ever flips the status, and `agent
/// wait`/`agent list`'s `*` must not treat that window as "already done".
pub(crate) fn still_going(store: &SqliteTaskStore, task: &AgentTask) -> bool {
    task.status == TaskStatus::Running || locks::is_running(store, &task.id)
}

struct ListOptions {
    all: bool,
    json: bool,
}

fn parse_list_options(args: &[String]) -> Result<ListOptions> {
    let mut options = ListOptions {
        all: false,
        json: false,
    };
    for arg in args {
        match arg.as_str() {
            "--all" => options.all = true,
            "--json" => options.json = true,
            other => bail!("unsupported option {other}"),
        }
    }
    Ok(options)
}

pub(crate) fn list(ctx: &Context, store: &SqliteTaskStore, args: &[String]) -> Result<()> {
    let options = parse_list_options(args)?;
    let now = now();
    let tasks = store.list()?;
    let (shown, hidden): (Vec<_>, Vec<_>) = tasks.into_iter().partition(|task| {
        options.all
            || !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled)
            || now - task.created_at < HIDE_FINISHED_AFTER_SECS
    });

    if options.json {
        let rows: Vec<Value> = shown
            .iter()
            .map(|task| {
                json!({
                    "id": task.id,
                    "status": summary::status_label(task.status),
                    "running": still_going(store, task),
                    "tokens_used": task.tokens_used,
                    "token_budget": task.token_budget,
                    "age_secs": (now - task.created_at).max(0),
                    "goal": task.goal,
                })
            })
            .collect();
        ctx.write_stdout(&serde_json::to_string_pretty(
            &json!({"tasks": rows, "hidden": hidden.len()}),
        )?)?;
        return Ok(());
    }

    for task in &shown {
        let marker = if still_going(store, task) { '*' } else { ' ' };
        ctx.write_stdout(&format!(
            "{marker} {}  {:<15} {:>4}  {:>6}/{:<6}  {}",
            task.id,
            summary::status_label(task.status),
            format_age(now, task.created_at),
            task.tokens_used,
            task.token_budget,
            goal_preview(&task.goal),
        ))?;
    }
    if !hidden.is_empty() {
        ctx.write_stdout(&format!(
            "({} hidden; `agent list --all` shows everything)",
            hidden.len()
        ))?;
    }
    Ok(())
}

fn render_event_line(event: &TaskEvent) -> String {
    match event.kind.as_str() {
        "tool_intent" => format!(
            "[{}] {} {}",
            event.sequence,
            summary::tool_name(Some(&event.data)),
            dsh_types::text::clamp_chars(
                event
                    .data
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                80
            )
        ),
        "tool_result" => {
            let name = summary::tool_name(event.data.get("call"));
            let failed = event
                .data
                .get("failed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let result = event
                .data
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!(
                "[{}] {name} -> {}  {}",
                event.sequence,
                if failed { "failed" } else { "ok" },
                dsh_types::text::clamp_chars(result, 80)
            )
        }
        other => format!(
            "[{}] {other} {}",
            event.sequence,
            dsh_types::text::clamp_chars(&event.data.to_string(), 120)
        ),
    }
}

#[derive(Debug)]
struct LogsOptions {
    follow: bool,
    json: bool,
    since: u64,
}

fn parse_logs_options(args: &[String]) -> Result<(&str, LogsOptions)> {
    let id = args.first().context("task ID required")?;
    let mut options = LogsOptions {
        follow: false,
        json: false,
        since: 0,
    };
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--follow" => options.follow = true,
            "--json" => options.json = true,
            "--since" => {
                index += 1;
                let value = args.get(index).context("--since needs a sequence number")?;
                options.since = value
                    .parse()
                    .with_context(|| format!("--since {value:?} is not a sequence number"))?;
            }
            other => bail!("unsupported option {other}"),
        }
        index += 1;
    }
    Ok((id, options))
}

/// Prints every event past `since`, returning the new high-water mark.
fn print_new_events(
    ctx: &Context,
    store: &SqliteTaskStore,
    id: &str,
    since: u64,
    json: bool,
) -> Result<u64> {
    let events = store.events(id)?;
    let mut since = since;
    let new_events: Vec<_> = events
        .into_iter()
        .filter(|event| event.sequence > since)
        .collect();
    for event in &new_events {
        if json {
            ctx.write_stdout(&serde_json::to_string(event)?)?;
        } else {
            ctx.write_stdout(&render_event_line(event))?;
        }
        since = since.max(event.sequence);
    }
    Ok(since)
}

pub(crate) fn logs(ctx: &Context, store: &SqliteTaskStore, args: &[String]) -> Result<()> {
    let (id, options) = parse_logs_options(args)?;
    // `store.events(id)` never errors for an unknown id - it just answers
    // "no events", which used to make a typo'd or omitted id (e.g. `agent
    // logs --follow` with the id left off, so `--follow` itself gets parsed
    // as the id) succeed silently with nothing printed, unlike `show`/
    // `cancel`/`wait`, which all reject an unknown id the same way this
    // does.
    store.load(id)?;
    let mut since = options.since;
    loop {
        since = print_new_events(ctx, store, id, since, options.json)?;
        if !options.follow {
            return Ok(());
        }
        let task = store.load(id)?;
        if !still_going(store, &task) {
            // One last pass: something may have been written between the
            // read above and this check.
            print_new_events(ctx, store, id, since, options.json)?;
            return Ok(());
        }
        if crate::process::signal::check_and_clear_sigint() {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[derive(Debug)]
struct WaitOptions {
    timeout_secs: Option<u64>,
}

fn parse_wait_options(args: &[String]) -> Result<(&str, WaitOptions)> {
    let id = args.first().context("task ID required")?;
    let mut options = WaitOptions { timeout_secs: None };
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--timeout" => {
                index += 1;
                let value = args.get(index).context("--timeout needs seconds")?;
                options.timeout_secs = Some(
                    value
                        .parse()
                        .with_context(|| format!("--timeout {value:?} is not seconds"))?,
                );
            }
            other => bail!("unsupported option {other}"),
        }
        index += 1;
    }
    Ok((id, options))
}

pub(crate) fn wait(ctx: &Context, store: &SqliteTaskStore, args: &[String]) -> Result<()> {
    let (id, options) = parse_wait_options(args)?;
    let deadline = options
        .timeout_secs
        .map(|secs| std::time::Instant::now() + Duration::from_secs(secs));
    loop {
        let task = store.load(id)?;
        if !still_going(store, &task) {
            ctx.write_stdout(&summary::task_summary(&task, &store.events(id)?))?;
            if super::task_completed(&task) {
                return Ok(());
            }
            bail!("task {id} stopped; inspect with agent show");
        }
        if let Some(deadline) = deadline
            && std::time::Instant::now() >= deadline
        {
            bail!(
                "timed out waiting for task {id}; it is still running - `agent wait {id}` again, or `agent show {id}`"
            );
        }
        if crate::process::signal::check_and_clear_sigint() {
            bail!("interrupted while waiting for task {id}");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests;
