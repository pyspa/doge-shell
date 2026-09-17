//! `agent run --detach`: hands a prepared task to a separate `dogesh -c
//! "agent run-detached <id>"` process and returns immediately.
//!
//! Reuses [`super::run_task`] exactly as cron's AI jobs do (`dsh/src/cron/
//! run_job.rs`) - this is not a third agent loop, only a third way to reach
//! the one that already exists. The two sides of the process boundary:
//!
//! - [`start`] runs in the interactive shell. It validates the task the same
//!   way an ordinary `agent run` would (so a doomed run - an empty goal, an
//!   exhausted time budget, a root that does not resolve - fails in front of the
//!   person who typed the command, not silently inside a process nobody is
//!   watching), persists it, and spawns the child.
//! - [`execute`] runs inside that child. It is what `agent run-detached
//!   <id>` (an internal action, not documented in `HELP` - the only caller
//!   is `start`) dispatches to.
//!
//! The child's stdout/stderr are kept, not discarded like cron's: a person
//! started this run and may want to look at it, where cron already has
//! `cron logs`. They land in `<agent state dir>/<task id>/run.log`, inside
//! the same per-task artifact directory `agent delete` already removes.

use super::{SqliteTaskStore, locks, unattended, validate, watchdog};
use anyhow::{Context as _, Result, bail};
use dsh_builtin::config_paths;
use dsh_builtin::shell_capabilities::AgentTaskStore;
use dsh_types::Context;
use dsh_types::agent::{AgentTask, TaskStatus};
use serde_json::json;
use std::fs::OpenOptions;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

/// Seconds of slack a detached run's watchdog leaves past its own time
/// budget before it gives up on `run_task` returning by itself. Unlike
/// cron's watchdog (`dsh/src/cron/run_job.rs`), there is no lease to stay
/// under - this is a flat grace period instead.
const DETACH_WATCHDOG_GRACE_SECS: u64 = 60;

fn detach_deadline_secs(time_budget_ms: u64) -> u64 {
    (time_budget_ms / 1000).saturating_add(DETACH_WATCHDOG_GRACE_SECS)
}

fn artifact_dir(id: &str) -> PathBuf {
    config_paths::agent_state_dir().join(id)
}

/// Refuses to run a task that has nothing left to do. Split out as a pure
/// check so it can be tested without a real store or `Shell`.
fn refuse_if_finished(task: &AgentTask) -> Result<()> {
    if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
        bail!("task {} already finished ({:?})", task.id, task.status);
    }
    Ok(())
}

/// Opens (creating if needed) a private, append-mode log file: `run.log`
/// accumulates across every detached run of the same task, the way a
/// person re-running `tail -f` on the same file expects.
fn open_private_log(path: &Path) -> Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))
}

/// Starts `task` as a detached run and returns as soon as the child exists.
///
/// Called from the interactive shell (`agent run --detach` / `agent resume
/// ID --detach`); `task` has already been built or loaded and its CLI
/// options applied, exactly as for a foreground run.
pub(crate) fn start(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &Arc<SqliteTaskStore>,
    mut task: AgentTask,
    reconcile: Option<String>,
) -> Result<()> {
    // Checked here, on the original status, before anything below
    // overwrites it: once this function sets `task.status =
    // TaskStatus::Interrupted` a few lines down, `execute()`'s own
    // `refuse_if_finished` call (inside the child) can never again observe
    // whatever this task's *real* status was - `Completed` and `Cancelled`
    // both get clobbered to `Interrupted` before the child ever loads the
    // row, which would otherwise let a already-finished task be silently
    // re-run in the background.
    refuse_if_finished(&task)?;
    validate::startable(shell, store, &task, reconcile.as_deref())?;

    // Advisory: gives an immediate, in-front-of-the-user error instead of a
    // detached child dying silently a moment later. The child's own
    // `admit_run` is what actually decides, so the lock is released right
    // back before spawning rather than held across the process boundary.
    //
    // Checked *before* anything below mutates or persists `task`: a task
    // that fails to admit here (busy, or no free slot) must be left exactly
    // as it was found - in particular, a task sitting at `InputRequired`
    // must keep its real status and its `stop_reason` (the one thing that
    // names the permission it is waiting on) rather than have both silently
    // overwritten by a `--detach` attempt that never actually starts.
    match locks::admit_run(shell, store, &task.id)? {
        locks::Admission::Admitted(lock) => drop(lock),
        locks::Admission::TaskBusy => bail!("task {} is already running", task.id),
        locks::Admission::NoFreeSlot => {
            bail!("another agent task is active; cancel it, wait, or raise AI_AGENT_MAX_CONCURRENT")
        }
    }

    if let Some(note) = reconcile {
        task.pending_operation = None;
        task.progress = format!("User reconciled interrupted operation: {note}");
    }
    // Left `Interrupted`, not `Running`: nothing is executing yet, and
    // `run_task` (called inside the child) is what transitions it to
    // `Running` once it actually starts.
    task.status = TaskStatus::Interrupted;
    task.stop_reason = None;
    store.save(&task, Some(("detach_requested", &json!(null))))?;

    let log_path = artifact_dir(&task.id).join("run.log");
    let stdout_file = open_private_log(&log_path)?;
    let stderr_file = stdout_file.try_clone()?;

    let child = crate::detached_child::spawn(
        &format!("agent run-detached {}", task.id),
        Stdio::from(stdout_file),
        Stdio::from(stderr_file),
    )?;
    let pid = child.id();
    // Reloaded, not the pre-spawn `task` still in hand: the child may
    // already have raced ahead of this line (loaded the row, taken its own
    // lock, started `run_task`) between the spawn above and this save, and
    // re-saving the stale snapshot would clobber whatever it already
    // recorded (e.g. `Running`) back to this function's own pre-spawn
    // `Interrupted`. Falls back to the pre-spawn snapshot only if the reload
    // itself fails, so this save still happens either way.
    let task = store.load(&task.id).unwrap_or(task);
    store.save(
        &task,
        Some((
            "detached",
            &json!({"pid": pid, "started_at": chrono::Utc::now().timestamp()}),
        )),
    )?;
    crate::detached_child::reap(child);

    ctx.write_stdout(&format!(
        "Task {} detached (pid {pid}); log at {}",
        task.id,
        config_paths::display_path(&log_path)
    ))?;
    Ok(())
}

/// Runs a task that [`start`] already prepared, inside the detached child.
///
/// The only thing that crossed the process boundary is `task_id`, validated
/// here so a corrupt argument cannot put anything else on a command line -
/// everything the run needs was already read back from the store the same
/// way cron's AI jobs do it.
pub(crate) fn execute(shell: &mut crate::shell::Shell, ctx: &Context, task_id: &str) -> Result<()> {
    uuid::Uuid::parse_str(task_id).context("task id is not a UUID")?;
    let store = Arc::new(SqliteTaskStore::open(&config_paths::agent_state_dir())?);

    let config = dsh_builtin::agent::resolved_config(shell);
    if let Some(key) = config.api_key() {
        store.remember_secret(key);
    }

    let _lock = match locks::admit_run(shell, &store, task_id)? {
        locks::Admission::Admitted(lock) => lock,
        locks::Admission::TaskBusy => bail!("task {task_id} is already running"),
        locks::Admission::NoFreeSlot => {
            let mut task = store.load(task_id)?;
            task.status = TaskStatus::Interrupted;
            task.stop_reason = Some(
                "no free agent execution slot (AI_AGENT_MAX_CONCURRENT); try again once another task finishes"
                    .to_string(),
            );
            store.save(
                &task,
                Some((
                    "stopped",
                    &json!({"status": task.status, "reason": task.stop_reason}),
                )),
            )?;
            bail!("no free execution slot for task {task_id}");
        }
    };
    store.recover_interrupted()?;

    let task = store.load(task_id)?;
    refuse_if_finished(&task)?;

    // `dsh -c` never connects MCP - it is an interactive service - so a task
    // that granted MCP calls would otherwise run with none of its tools.
    if !task.grant.mcp_calls.is_empty() {
        unattended::connect_mcp(shell);
    }

    let watchdog = watchdog::arm(detach_deadline_secs(task.time_budget_ms));
    let report = super::run_task(shell, ctx, &store, task, None);
    watchdog::disarm(&watchdog);
    let report = report?;

    if !report.succeeded {
        bail!("task {} stopped; inspect with agent show", report.id);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
