//! Explicit `agent` entry point: parses `agent ...` argv and runs one task.
//!
//! The SQLite store itself lives in [`store`]; per-action work in the sibling
//! modules below.
use anyhow::{Context as _, Result, bail};
use dsh_builtin::{
    agent::AgentRuntime,
    shell_capabilities::AgentTaskStore,
};
use dsh_types::{
    Context,
    agent::{AgentTask, TaskGrant, TaskStatus, Verification},
};
use parking_lot::Mutex;
use self::store::tag_failure;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) mod approve;
pub(crate) mod blocked;
pub(crate) mod cli;
pub(crate) mod detach;
pub(crate) mod doctor;
pub(crate) mod locks;
pub(crate) mod notice;
pub(crate) mod profiles;
pub(crate) mod store;
pub(crate) mod summary;
pub(crate) mod unattended;
pub(crate) mod validate;
pub(crate) mod watch;
pub(crate) mod watchdog;

pub use store::SqliteTaskStore;
pub(crate) use store::TaskFailure;



/// Default cumulative time budget in seconds (`--timeout`, then
/// `AI_AGENT_TIMEOUT_SECS`).
pub(crate) const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 1800;

const HELP: &str = "agent run [--timeout SECONDS] [--check TEXT] [--write DIR] [--read DIR] [--allow-command EXACT] [--profile NAME] [--allow-mcp ENTRY] [--sandbox] [--network HOST] [--env NAME] [--detach|-d] [--dry-run] -- GOAL\nagent resume ID [--timeout SECONDS] [--reconcile TEXT] [--allow-command EXACT] [--profile NAME] [--detach|-d] [--dry-run]\nagent retry ID [--timeout SECONDS] [--check TEXT] [--reconcile TEXT] [--allow-command EXACT] [--profile NAME] [--detach|-d] [--dry-run]\nagent approve ID [--reconcile TEXT] [--dry-run]\nagent profiles\nagent list [--all] [--json] | logs ID [--follow] [--json] | wait ID [--timeout SECONDS]\nagent show ID [--summary] | cancel ID | delete ID | doctor [--json]\nagent respond ID SERVER REMOTE_TASK_ID JSON_INPUT_RESPONSES\n--profile expands to exact commands (see `agent profiles`); resume/retry also accept grant options. `approve` asks once, widens the grant by the one refusal the task is stuck on, and resumes in the foreground. --dry-run prints the expanded grant without starting. --detach (-d) starts the task in a separate process and returns immediately; see `agent list`/`agent logs`/`agent wait` to follow it.\nBudget: --timeout, else AI_AGENT_TIMEOUT_SECS (shell variable, then environment), else 1800s. AI_AGENT_MAX_CONCURRENT (default 1) bounds how many tasks - detached or not - may run at once.\n";

/// Whether a turn that just ended needs an explicit
/// `AgentLifecycleManager::report_blocked` call rather than letting
/// `TurnGuard::drop` report `Idle` as usual.
///
/// A persistent task's approval requests never reach the interactive
/// `confirm_action` bracket: `dsh/src/proxy/mod.rs`'s `agent_runtime.is_some()`
/// branch denies without asking, since nothing is watching an unattended
/// task. A denial is returned to the model as a tool-result error the turn
/// works around (recorded via `AgentRuntime::note_denial`); "blocked" here
/// is only the leftover state where the turn ends still needing a person -
/// the three-strikes guard's `TaskStatus::InputRequired` - detected after
/// `execute_chat_message` has already returned and reported explicitly,
/// before `turn` drops, so `TurnGuard::drop` sees it and skips forcing
/// `Idle` over it. A task that works around refusals until it cannot proceed
/// lands `Interrupted` with the grant hint instead, which needs no lifecycle
/// report.
/// Whether a finished task counts as having succeeded.
///
/// `AgentTask::verified()` is false whenever no `--check` criteria were ever
/// given (its `!criteria.is_empty()` guard) - a task run without any is not
/// thereby unverifiable, it simply has nothing to verify, and `Completed`
/// alone is the answer. Only a task that *was* given criteria has to have
/// them all pass.
fn task_completed(task: &AgentTask) -> bool {
    task.status == TaskStatus::Completed && (task.criteria.is_empty() || task.verified())
}

fn blocked_reason_for(status: TaskStatus, stop_reason: Option<&str>) -> Option<String> {
    (status == TaskStatus::InputRequired).then(|| {
        stop_reason
            .map(str::to_string)
            .unwrap_or_else(|| "agent task needs approval".to_string())
    })
}

/// A setting, shell variable first and process environment second.
///
/// The order matters and is the same one `chatgpt::load_openai_config` uses;
/// a new key that only reads `std::env` would be invisible to `var`.
fn setting(shell: &mut crate::shell::Shell, key: &str) -> Option<String> {
    use dsh_builtin::ShellProxy;
    shell.get_var(key).or_else(|| std::env::var(key).ok())
}

pub fn command(shell: &mut crate::shell::Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    use dsh_builtin::ShellProxy;
    if argv.len() < 2 || matches!(argv[1].as_str(), "help" | "--help" | "-h") {
        ctx.write_stdout(HELP)?;
        return Ok(());
    }
    if shell.agent_runtime.is_some() {
        bail!("nested agent invocation is not allowed");
    }
    // Internal - the only caller is `detach::start`, and it does not print
    // `HELP`-shaped errors, so it is dispatched before anything else (the
    // same placement `cron run-job` uses for the same reason).
    if argv[1] == "run-detached" {
        let id = argv.get(2).context("task ID required")?;
        return detach::execute(shell, ctx, id);
    }
    let store = Arc::new(SqliteTaskStore::open(
        &dsh_builtin::config_paths::agent_state_dir(),
    )?);
    let config = dsh_builtin::agent::resolved_config(shell);
    if let Some(key) = config.api_key()
        && key.len() >= 4
    {
        store.secrets.lock().push(key.to_string());
    }
    store.recover_interrupted()?;
    let action = argv[1].as_str();
    if action == "list" {
        return cli::list(ctx, &store, &argv[2..]);
    }
    if action == "logs" {
        return cli::logs(ctx, &store, &argv[2..]);
    }
    if action == "wait" {
        return cli::wait(ctx, &store, &argv[2..]);
    }
    if action == "doctor" {
        return doctor::run(shell, ctx, &store, &argv[2..]);
    }
    if action == "profiles" {
        for (name, description) in profiles::list() {
            let commands = profiles::expand(name).unwrap_or(&[]);
            ctx.write_stdout(&format!("{name}: {description}\n  {}", commands.join(", ")))?;
        }
        return Ok(());
    }
    if action == "respond" {
        let id = argv.get(2).context("task ID required")?;
        let server = argv.get(3).context("server label required")?;
        let remote = argv.get(4).context("remote task ID required")?;
        let input: Value =
            serde_json::from_str(argv.get(5).context("JSON inputResponses required")?)?;
        let _lock = locks::try_lock_task(&store, id)?
            .context("this task is currently running; cancel it or wait")?;
        let mut task = store.load(id)?;
        if task.pending_operation.is_some() {
            bail!("reconcile the previous operation before sending further input");
        }
        if !dsh_builtin::agent::has_remote_task(&store.events(id)?, server, remote) {
            bail!("remote task does not belong to this task");
        }
        let manager = shell
            .environment
            .read()
            .integration_state
            .mcp_manager
            .clone();
        task.pending_operation =
            Some(json!({"server":server,"task_id":remote,"operation":"task_update"}));
        store.save(
            &task,
            Some((
                "remote_input_intent",
                &task.pending_operation.clone().unwrap(),
            )),
        )?;
        manager
            .read()
            .task_operation(
                server,
                "task_update",
                json!({"taskId":remote,"inputResponses":input}),
                &|| false,
            )
            .map_err(anyhow::Error::msg)?;
        task.pending_operation = None;
        store.save(
            &task,
            Some(("remote_input", &json!({"server":server,"task_id":remote}))),
        )?;
        ctx.write_stdout("Input delivered; resume the agent task to continue polling")?;
        return Ok(());
    }
    if action == "approve" {
        return approve::approve(shell, ctx, &store, &argv[2..]);
    }
    if matches!(action, "show" | "cancel" | "delete") {
        let id = argv.get(2).context("task ID required")?;
        let task = store.load(id)?;
        match action {
            // The default stays the full JSON dump: it is what
            // `docs/ai/skills/dsh-cron/references/troubleshooting.md` tells a
            // person to copy an exact `--allow-mcp` approval key out of, and
            // that key lives in `events`, not in `--summary`'s report.
            "show" if argv.get(3).map(String::as_str) == Some("--summary") => {
                ctx.write_stdout(&summary::task_summary(&task, &store.events(id)?))?
            }
            "show" => ctx.write_stdout(&serde_json::to_string_pretty(
                &json!({"task":task,"events":store.events(id)?}),
            )?)?,
            "cancel" => {
                store.cancel(id)?;
                ctx.write_stdout(
                    "Cancellation requested. Remote effects may already have occurred.",
                )?;
            }
            _ => {
                let _lock = locks::try_lock_task(&store, id)?
                    .context("this task is currently running; cancel it or wait")?;
                store.delete(id)?;
                ctx.write_stdout("Task and recorded output deleted")?;
            }
        }
        if matches!(action, "cancel" | "delete") {
            // The user just explicitly resolved a pending decision without
            // going through `agent resume` - e.g. a foreground `agent run`
            // blocked, returned control to this same interactive shell with
            // `TaskStatus::InputRequired`, and the user chose to give up on
            // it instead. That left this shell's lifecycle state at
            // `Blocked` (see `blocked_reason_for`'s call site below); it must
            // not go on claiming a human's attention is still needed. A
            // harmless no-op if the state wasn't `Blocked` (or wasn't this
            // task) to begin with - `report_idle` dedupes.
            crate::agent_lifecycle::current(shell).report_idle();
        }
        return Ok(());
    }
    if !matches!(action, "run" | "resume" | "retry") {
        bail!("unknown agent action; {HELP}");
    }

    let root = shell.get_current_dir()?.canonicalize()?;
    let mut task = if action == "resume" {
        store.load(argv.get(2).context("task ID required")?)?
    } else if action == "retry" {
        let source_id = argv.get(2).context("task ID required")?;
        let source = store.load(source_id)?;
        if matches!(source.status, TaskStatus::Completed | TaskStatus::Cancelled) {
            bail!("task {source_id} already finished; start a new task with `agent run`");
        }
        // Never clone a live run: without this, a retry of a `Running` task
        // would duplicate the same work when the
        // concurrency ceiling allows it.
        if source.status == TaskStatus::Running
            || locks::try_lock_task(&store, &source.id)?.is_none()
        {
            bail!("task {source_id} is already running; `agent wait`/`agent cancel` it before retrying");
        }
        AgentTask {
            id: uuid::Uuid::new_v4().to_string(),
            goal: source.goal.clone(),
            root: source.root.clone(),
            status: TaskStatus::Interrupted,
            grant: source.grant.clone(),
            criteria: source
                .criteria
                .iter()
                .map(|c| Verification {
                    criterion: c.criterion.clone(),
                    evidence_event: None,
                    passed: false,
                })
                .collect(),
            plan: vec![],
            progress: format!("retried from {source_id}"),
            tokens_used: 0,
            time_budget_ms: source.time_budget_ms,
            elapsed_ms: 0,
            stop_reason: None,
            checkpoint: None,
            pending_operation: source.pending_operation.clone(),
            created_at: chrono::Utc::now().timestamp(),
        }
    } else {
        AgentTask {
            id: uuid::Uuid::new_v4().to_string(),
            goal: String::new(),
            root: root.clone(),
            status: TaskStatus::Interrupted,
            grant: TaskGrant {
                read_roots: vec![root],
                ..Default::default()
            },
            criteria: vec![],
            plan: vec![],
            progress: String::new(),
            tokens_used: 0,
            time_budget_ms: setting(shell, "AI_AGENT_TIMEOUT_SECS")
                .and_then(|s| s.parse::<u64>().ok())
                .and_then(|s| s.checked_mul(1000))
                .unwrap_or(DEFAULT_AGENT_TIMEOUT_SECS * 1000),
            elapsed_ms: 0,
            stop_reason: None,
            checkpoint: None,
            pending_operation: None,
            created_at: chrono::Utc::now().timestamp(),
        }
    };
    let mut index = if action == "run" { 2 } else { 3 };
    let mut reconcile = None;
    let mut detach = false;
    let mut dry_run = false;
    let mut profile_names: Vec<String> = vec![];
    while index < argv.len() {
        let option = &argv[index];
        index += 1;
        if option == "--" && action == "run" {
            task.goal = argv[index..].join(" ");
            break;
        }
        if option == "--sandbox" && action == "run" {
            task.grant.sandbox = true;
            continue;
        }
        if option == "--detach" || option == "-d" {
            detach = true;
            continue;
        }
        if option == "--dry-run" {
            dry_run = true;
            continue;
        }
        let value = argv.get(index).context("option value required")?;
        index += 1;
        if option == "--profile" && matches!(action, "run" | "resume" | "retry") {
            profile_names.push(value.clone());
            continue;
        }
        // Grant-shaped options (`--read`/`--write`/`--allow-command`/
        // `--allow-mcp`/`--network`/`--env`) are shared with `cron add
        // --agent`, so an unattended job's grant validates exactly the way
        // an interactive one does.
        if matches!(action, "run" | "resume" | "retry")
            && dsh_builtin::agent::grant::apply_grant_option(&mut task.grant, option, value)?
        {
            continue;
        }
        match option.as_str() {
            "--timeout" => {
                task.time_budget_ms = value
                    .parse::<u64>()?
                    .checked_mul(1000)
                    .context("timeout too large")?
            }
            "--reconcile" if matches!(action, "resume" | "retry") => {
                reconcile = Some(value.clone())
            }
            "--check" if matches!(action, "run" | "retry") => task.criteria.push(Verification {
                criterion: value.clone(),
                evidence_event: None,
                passed: false,
            }),
            _ => bail!("unsupported option {option}"),
        }
    }
    if !profile_names.is_empty() {
        profiles::apply(&mut task.grant, &profile_names)?;
    }
    if dry_run {
        ctx.write_stdout(&format!(
            "goal: {}\ncommands: {}\nread: {}\nwrite: {}\ntokens_used: {}\ntimeout_secs: {}\ndetach: {detach}",
            task.goal,
            if task.grant.commands.is_empty() {
                "(none)".to_string()
            } else {
                task.grant.commands.join(", ")
            },
            task.grant
                .read_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            if task.grant.write_roots.is_empty() {
                "(none)".to_string()
            } else {
                task.grant
                    .write_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            task.tokens_used,
            task.time_budget_ms / 1000,
        ))?;
        // Fail fast like `detach::start`'s parent-side check: a preview that
        // passes while the real run would immediately fail (empty goal,
        // exhausted time budget, unreconciled operation) is worse than no preview.
        if let Err(error) = validate::startable(shell, &store, &task, reconcile.as_deref()) {
            bail!("dry-run validation failed: {error:#}");
        }
        return Ok(());
    }
    if detach {
        return detach::start(shell, ctx, &store, task, reconcile);
    }
    admit_and_run(shell, ctx, &store, task, reconcile)
}

/// Admits one prepared task under the global execution lock and runs it to
/// a stopping point, turning a non-success into an error for the
/// interactive caller. Shared by `command`'s run/resume/retry tail and
/// `approve` (which resumes right after widening the grant).
fn admit_and_run(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &Arc<SqliteTaskStore>,
    task: AgentTask,
    reconcile: Option<String>,
) -> Result<()> {
    let _lock = match locks::admit_run(shell, store, &task.id)? {
        locks::Admission::Admitted(lock) => lock,
        locks::Admission::TaskBusy => bail!("task {} is already running", task.id),
        locks::Admission::NoFreeSlot => {
            bail!("another agent task is active; cancel it, wait, or raise AI_AGENT_MAX_CONCURRENT")
        }
    };
    let report = run_task(shell, ctx, store, task, reconcile)?;
    if !report.succeeded {
        bail!("task {} stopped; inspect with agent show", report.id);
    }
    Ok(())
}

/// What one task run ended up doing.
///
/// `command` turns a non-success into an error because a person typed
/// `agent run` and is waiting for an exit code. Cron cannot: an unattended run
/// that needs a permission is a case to record, not a failure to propagate, so
/// the shared path reports and lets each caller decide.
pub(crate) struct TaskRunReport {
    pub id: String,
    pub status: TaskStatus,
    pub stop_reason: Option<String>,
    pub tokens_used: u64,
    /// Completed **and** every criterion verified against a recorded result.
    pub succeeded: bool,
}

/// Runs one prepared task to a stopping point.
///
/// Split out of [`command`] so `cron` can start a task from a stored spec
/// instead of rebuilding a command line: the goal and the grant travel as
/// values and never pass through a shell parser. Everything else - the
/// lifecycle reporting, the working-directory restore, the single
/// `agent_runtime` slot - is identical, deliberately, so an unattended run is
/// the same run a person would have got.
pub(crate) fn run_task(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &Arc<SqliteTaskStore>,
    mut task: AgentTask,
    reconcile: Option<String>,
) -> Result<TaskRunReport> {
    use dsh_builtin::ShellProxy;
    validate::startable(shell, store, &task, reconcile.as_deref())?;
    if let Some(note) = reconcile {
        task.pending_operation = None;
        task.progress = format!("User reconciled interrupted operation: {note}");
        store
            .save(&task, Some(("reconciled", &json!(note))))
            .map_err(|error| tag_failure(TaskFailure::StateUnusable, error))?;
    }
    // Explicit resume is the only path that may clear a cancellation.
    task.status = TaskStatus::Running;
    task.stop_reason = None;
    task = store
        .resume(&task, Some(("started", &Value::Null)))
        .map_err(|error| tag_failure(TaskFailure::StateUnusable, error))?
        .task;
    let old_cwd = shell.get_current_dir()?;
    ctx.write_stdout(&format!("Task {}", task.id))?;
    if let Err(error) = shell.changepwd(&task.root.to_string_lossy()) {
        task.status = TaskStatus::Failed;
        task.stop_reason = Some(error.to_string());
        // Tagged `StateUnusable`, not left as a bare `?`: an untagged error
        // here would make `run_job::failure_reason` default this to
        // `RunReason::Transient` instead of blocking the job, even though a
        // task store that cannot even record "the root changed" is exactly
        // the "continuing to retry is not obviously safe" case
        // `TaskFailure::StateUnusable` exists for.
        store
            .save(&task, None)
            .map_err(|save_error| tag_failure(TaskFailure::StateUnusable, save_error))?;
        let _ = shell.changepwd(&old_cwd.to_string_lossy());
        return Err(anyhow::Error::new(TaskFailure::RootChanged).context(format!("{error}")));
    }
    let goal = task.goal.clone();
    let id = task.id.clone();
    shell.agent_runtime = Some(Arc::new(Mutex::new(AgentRuntime::new(
        task,
        Arc::clone(store) as Arc<dyn AgentTaskStore>,
    ))));
    let lifecycle = crate::agent_lifecycle::current(shell);
    let turn = lifecycle.begin_turn();
    let status = dsh_builtin::execute_chat_message(ctx, shell, &goal, None);
    let mut completed = false;
    // Unattended approval requests never reach the interactive
    // `confirm_action` bracket (`dsh/src/proxy/mod.rs`'s
    // `agent_runtime.is_some()` branch denies without asking anyone) - so
    // "blocked" here is a leftover state where the turn ends still needing a
    // person (the three-strikes guard's `TaskStatus::InputRequired`), not one
    // that resolves before the turn does. Reported explicitly, before `turn`
    // drops, so `TurnGuard::drop` sees it and skips forcing `Idle` over it.
    let mut blocked_reason = None;
    let mut final_status = TaskStatus::Interrupted;
    let mut stop_reason = None;
    let mut tokens_used = 0;
    let cleanup = if let Some(runtime) = shell.agent_runtime.take() {
        let mut runtime = runtime.lock();
        let result = if runtime.task.status == TaskStatus::Running {
            runtime.finish(false, Some(format!("chat exited: {status:?}")))
        } else {
            Ok(())
        };
        completed = task_completed(&runtime.task);
        final_status = runtime.task.status;
        stop_reason = runtime.task.stop_reason.clone();
        tokens_used = runtime.task.tokens_used;
        blocked_reason =
            blocked_reason_for(runtime.task.status, runtime.task.stop_reason.as_deref());
        result.and_then(|()| {
            ctx.write_stdout(&format!(
                "Task {id}: {:?} — {}",
                runtime.task.status,
                runtime
                    .task
                    .stop_reason
                    .as_deref()
                    .unwrap_or("all criteria verified")
            ))
        })
    } else {
        Ok(())
    };
    if let Some(reason) = blocked_reason {
        lifecycle.report_blocked(reason);
    }
    drop(turn);
    let restored = shell.changepwd(&old_cwd.to_string_lossy());
    cleanup?;
    restored?;

    Ok(TaskRunReport {
        id,
        status: final_status,
        stop_reason,
        tokens_used,
        // The model saying it is done is not the same as it being done: a task
        // counts as succeeded only when the chat loop exited cleanly *and*
        // every criterion was verified against a recorded tool result.
        succeeded: status == dsh_types::ExitStatus::ExitedWith(0) && completed,
    })
}

#[cfg(test)]
mod tests;
