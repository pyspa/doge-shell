//! The in-session poller that turns a detached task finishing into a notice
//! above the prompt (`dsh/src/repl/loop_handlers.rs`) and a count on the
//! status line (`dsh/src/repl/status_line.rs`).
//!
//! Modelled on `dsh/src/cron/runner.rs`, whose own doc comment names exactly
//! this gap for cron ("per-run REPL notices... needs its own
//! watermark-and-poll design") - this is that design, built for agent tasks
//! first because `--detach` (`dsh/src/agent/detach.rs`) has no other way to
//! tell a person it finished.

use super::locks;
use dsh_builtin::shell_capabilities::AgentTaskStore as _;
use dsh_types::agent::{AgentHealth, AgentTask, TaskStatus};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// Poll interval while at least one detached task is still running - short
/// enough that a notice feels prompt, long enough not to hammer SQLite.
const ACTIVE_INTERVAL_SECS: u64 = 2;
/// Poll interval otherwise, matching cron's own idle ceiling
/// (`dsh/src/cron/runner.rs`'s `MAX_IDLE_SECS`) so the two background
/// pollers settle into the same rhythm when both are quiet.
const IDLE_INTERVAL_SECS: u64 = 60;

/// Decides which task transitions are worth a notice, and renders them.
///
/// A pure function of the previous scan's remembered statuses: no store, no
/// clock, so it is testable without SQLite or tokio. `seen` is updated in
/// place (the caller's memory of "what this session already knows"), and
/// `detached` restricts notices to tasks a `--detach` (or cron) run actually
/// started unattended - a foreground `agent run` already prints its own
/// result when it finishes, and notifying about it too would be a duplicate.
pub(crate) fn notices_for(
    seen: &mut HashMap<String, TaskStatus>,
    tasks: &[AgentTask],
    detached: &HashSet<String>,
    first_scan: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    for task in tasks {
        let previous = seen.insert(task.id.clone(), task.status);
        if first_scan || !detached.contains(&task.id) {
            continue;
        }
        if previous == Some(TaskStatus::Running) && task.status != TaskStatus::Running {
            lines.push(super::notice::render(task));
        }
    }
    lines
}

/// Whether `id` has ever been started with `agent run --detach` (or as a
/// cron AI job, which uses the same `detached_child::spawn` machinery) -
/// found once per task id, the first time it is seen, rather than every
/// scan: whether a task is detached never changes after it is created.
fn has_detached_event(store: &super::SqliteTaskStore, id: &str) -> bool {
    store
        .events(id)
        .map(|events| events.iter().any(|event| event.kind == "detached"))
        .unwrap_or(false)
}

/// `DOGESH_AGENT_WATCH`: whether the watcher runs at all. Default on -
/// `0`/`false`/`off`/`no` (case-insensitive) opts out, matching every other
/// on-by-default toggle in this codebase (`resolve_stream_enabled` and
/// friends in `dsh-builtin/src/chatgpt/settings.rs`).
pub(crate) fn resolve_enabled(shell: &mut crate::shell::Shell) -> bool {
    match super::setting(shell, "DOGESH_AGENT_WATCH") {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// `DOGESH_AGENT_WATCH_INTERVAL_SECS`: how often the watcher scans while at
/// least one detached task is running, clamped to 1..=60 so a typo cannot
/// turn this into a busy loop or an effectively-disabled watcher.
pub(crate) fn resolve_active_interval_secs(shell: &mut crate::shell::Shell) -> u64 {
    super::setting(shell, "DOGESH_AGENT_WATCH_INTERVAL_SECS")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(1, 60))
        .unwrap_or(ACTIVE_INTERVAL_SECS)
}

/// Runs until the task is aborted (on `Repl` drop). Errors from one scan are
/// logged and do not end the loop, the same tolerance
/// `cron::runner::cron_runner_task` gives a transient store hiccup.
pub(crate) async fn agent_watch_task(
    store: Arc<super::SqliteTaskStore>,
    health: Arc<parking_lot::RwLock<AgentHealth>>,
    pending: Arc<parking_lot::Mutex<Vec<String>>>,
    active_interval_secs: u64,
) {
    let mut seen: HashMap<String, TaskStatus> = HashMap::new();
    let mut detached: HashSet<String> = HashSet::new();
    let mut first_scan = true;

    loop {
        let tasks = match store.list() {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::warn!("agent: watch task could not list tasks: {error}");
                tokio::time::sleep(Duration::from_secs(IDLE_INTERVAL_SECS)).await;
                continue;
            }
        };

        for task in &tasks {
            if !seen.contains_key(&task.id) && has_detached_event(&store, &task.id) {
                detached.insert(task.id.clone());
            }
        }

        let notices = notices_for(&mut seen, &tasks, &detached, first_scan);
        first_scan = false;
        if !notices.is_empty() {
            pending.lock().extend(notices);
        }

        // `agent delete` removes a task's row (and thus, from the next
        // `store.list()` on, its id from `tasks`) but has no way to reach
        // into this task's own `seen`/`detached` memory - without this, a
        // long session that creates and deletes many tasks over time would
        // grow both maps by one entry per task ever seen, never shrinking.
        let live_ids: HashSet<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
        seen.retain(|id, _| live_ids.contains(id.as_str()));
        detached.retain(|id| live_ids.contains(id.as_str()));

        *health.write() = AgentHealth {
            running: tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Running)
                .count(),
            input_required: tasks
                .iter()
                .filter(|task| task.status == TaskStatus::InputRequired)
                .count(),
        };

        let still_active = tasks
            .iter()
            .any(|task| detached.contains(&task.id) && locks::is_running(&store, &task.id));
        let sleep_secs = if still_active {
            active_interval_secs
        } else {
            IDLE_INTERVAL_SECS
        };
        tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
    }
}

#[cfg(test)]
mod tests;
