//! Shell-owned SQLite task store and explicit `agent` entry point.
use anyhow::{Context as _, Result, bail};
use dsh_builtin::{
    agent::AgentRuntime,
    shell_capabilities::{AgentTaskSave, AgentTaskStore},
};
use dsh_types::{
    Context,
    agent::{AgentTask, TaskEvent, TaskGrant, TaskStatus, Verification},
};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    fs::{DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) mod blocked;
pub(crate) mod cli;
pub(crate) mod detach;
pub(crate) mod doctor;
pub(crate) mod locks;
pub(crate) mod notice;
pub(crate) mod profiles;
pub(crate) mod summary;
pub(crate) mod unattended;
pub(crate) mod validate;
pub(crate) mod watch;
pub(crate) mod watchdog;

/// Why [`run_task`] failed before it could even produce a [`TaskRunReport`],
/// tagged on the returned `anyhow::Error` so a caller that needs to act on
/// *which* kind of failure this was - `cron`'s `run_job::execute`, mapping to
/// a [`dsh_types::cron::job::RunReason`] - can recover it without matching on
/// message text.
///
/// Attached via `.context(...)`: `anyhow::Error::new(TaskFailure::X).context("human
/// message")` keeps the human-readable message on top (what `{}`/`to_string()`
/// show, unchanged from before this existed) while this marker sits one link
/// down the chain, found with
/// `error.chain().find_map(|c| c.downcast_ref::<TaskFailure>())`. Never
/// matched on directly here - `agent run`'s own error reporting is exactly
/// the display text, untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskFailure {
    /// The task's root, or a granted read/write root, no longer resolves to
    /// what it did when the task was created - deleted, or a symlink now
    /// pointing elsewhere.
    RootChanged,
    /// A previous operation's outcome was never confirmed and no
    /// `--reconcile` was given to settle it.
    Reconcile,
    /// A prerequisite the task cannot supply for itself - a missing sandbox
    /// runtime, an empty goal, or an exhausted budget - not something a
    /// retry would fix on its own.
    Config,
    /// The task store itself could not be read or written - the one case
    /// where continuing to retry is not obviously safe.
    StateUnusable,
}

impl std::fmt::Display for TaskFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RootChanged => "task root changed",
            Self::Reconcile => "unreconciled previous operation",
            Self::Config => "task misconfigured",
            Self::StateUnusable => "task store unusable",
        })
    }
}

impl std::error::Error for TaskFailure {}

/// Tags `error` as a [`TaskFailure`] of kind `failure`, keeping `error`'s own
/// `Display` text as the human-readable context - the same shape every
/// `run_task` call site used to spell out by hand (`anyhow::Error::new(TaskFailure::X)
/// .context(error.to_string())`). Only fits the plain "wrap this error
/// as-is" case; a call site that needs to prepend its own message keeps
/// writing that out directly instead of forcing a description parameter here.
fn tag_failure<E: std::fmt::Display>(failure: TaskFailure, error: E) -> anyhow::Error {
    anyhow::Error::new(failure).context(error.to_string())
}

pub struct SqliteTaskStore {
    connection: Mutex<Connection>,
    root: PathBuf,
    secrets: Mutex<Vec<String>>,
}
impl SqliteTaskStore {
    pub fn open(root: &Path) -> Result<Self> {
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        let meta = std::fs::symlink_metadata(root)?;
        if meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
            bail!("agent state directory must be a private directory (0700)");
        }
        let path = root.join("tasks.sqlite3");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            bail!("agent database must be private (0600)");
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,task_id TEXT NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL);")?;
        Ok(Self {
            connection: Mutex::new(connection),
            root: root.canonicalize()?,
            secrets: Mutex::new(
                std::env::vars()
                    .filter(|(key, value)| {
                        dsh_types::safety_policy::is_sensitive_key(key) && value.len() >= 8
                    })
                    .map(|(_, value)| value)
                    .collect(),
            ),
        })
    }
    fn cancel(&self, id: &str) -> Result<()> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let body: String =
            tx.query_row("SELECT body FROM tasks WHERE id=?1", [id], |row| row.get(0))?;
        let mut task: AgentTask = serde_json::from_str(&body)?;
        task.status = TaskStatus::Cancelled;
        task.stop_reason = Some("cancelled by user".into());
        tx.execute(
            "UPDATE tasks SET body=?2 WHERE id=?1",
            params![id, serde_json::to_string(&task)?],
        )?;
        tx.execute(
            "INSERT INTO events(task_id,kind,data) VALUES(?1,'cancelled','null')",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Adds a value to the set masked out of everything this store writes.
    ///
    /// `command` does this for the API key; cron has to do the same before it
    /// starts an unattended task, because that task's events outlive the
    /// session that produced them.
    pub(crate) fn remember_secret(&self, value: &str) {
        if value.len() >= 4 {
            self.secrets.lock().push(value.to_string());
        }
    }
    /// Recovers every `Running` task no live process still holds the lock
    /// for.
    ///
    /// Checked per task (`locks::try_lock_task`), not globally: a task is
    /// only recovered once *its own* lock is proven free, so a live run of
    /// one task can never cause a different, still-running task to be
    /// marked `Interrupted` out from under it.
    pub(crate) fn recover_interrupted(&self) -> Result<()> {
        let tasks = self.list()?;
        for mut task in tasks.iter().cloned() {
            if task.status != TaskStatus::Running {
                continue;
            }
            // A live owner holds this task's own lock - only recover once
            // that is proven false, by actually taking it ourselves.
            let Some(lock) = locks::try_lock_task(self, &task.id)? else {
                continue;
            };
            let jobs = dsh_builtin::agent::unfinished_local_jobs(&self.events(&task.id)?);
            if !jobs.is_empty() && task.pending_operation.is_none() {
                task.pending_operation = Some(
                    json!({"unfinished_jobs":jobs,"reason":"previous processes may still be running; inspect their effects before resuming"}),
                );
            }
            task.status = TaskStatus::Interrupted;
            task.stop_reason = Some(RECOVERED_STOP_REASON.to_string());
            self.save(&task, Some(("recovered", &Value::Null)))?;
            drop(lock);
        }
        let known_ids: Vec<String> = tasks.into_iter().map(|task| task.id).collect();
        locks::prune_orphaned_locks(self, &known_ids);
        Ok(())
    }

    fn persist(
        &self,
        task: &AgentTask,
        event: Option<(&str, &Value)>,
        explicit_resume: bool,
    ) -> Result<AgentTaskSave> {
        if explicit_resume && task.status != TaskStatus::Running {
            bail!("an explicit resume must transition the task to running");
        }

        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous: Option<AgentTask> = tx
            .query_row("SELECT body FROM tasks WHERE id=?1", [&task.id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .map(|body| serde_json::from_str(&body))
            .transpose()?;

        let mut effective = task.clone();
        if !explicit_resume
            && let Some(previous) = previous
            && previous.status == TaskStatus::Cancelled
        {
            // A late checkpoint/result is still useful, but cancellation is a
            // monotonic host-owned state until the user explicitly resumes.
            effective.status = TaskStatus::Cancelled;
            effective.stop_reason = previous.stop_reason;
        }

        let mut body = serde_json::to_value(&effective)?;
        redact(&mut body, &self.secrets.lock());
        tx.execute(
            "INSERT INTO tasks(id,body) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            params![effective.id, body.to_string()],
        )?;

        let sequence = if let Some((kind, data)) = event {
            let mut data = if kind == "stopped" {
                json!({"status":effective.status,"reason":effective.stop_reason})
            } else {
                data.clone()
            };
            redact(&mut data, &self.secrets.lock());
            tx.execute(
                "INSERT INTO events(task_id,kind,data) VALUES(?1,?2,?3)",
                params![effective.id, kind, data.to_string()],
            )?;
            tx.last_insert_rowid() as u64
        } else {
            0
        };
        tx.commit()?;
        Ok(AgentTaskSave {
            sequence,
            task: effective,
        })
    }
}
fn redact(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => {
            *text = dsh_types::safety_policy::redact_sensitive_text(text);
            for secret in secrets {
                *text = text.replace(secret, "***");
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| redact(v, secrets)),
        Value::Object(values) => values.values_mut().for_each(|v| redact(v, secrets)),
        _ => {}
    }
}
impl AgentTaskStore for SqliteTaskStore {
    fn save(&self, task: &AgentTask, event: Option<(&str, &Value)>) -> Result<AgentTaskSave> {
        self.persist(task, event, false)
    }

    fn resume(&self, task: &AgentTask, event: Option<(&str, &Value)>) -> Result<AgentTaskSave> {
        self.persist(task, event, true)
    }
    fn load(&self, id: &str) -> Result<AgentTask> {
        let body: String = self
            .connection
            .lock()
            .query_row("SELECT body FROM tasks WHERE id=?1", [id], |r| r.get(0))
            .context("unknown task")?;
        Ok(serde_json::from_str(&body)?)
    }
    fn list(&self) -> Result<Vec<AgentTask>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare("SELECT body FROM tasks ORDER BY rowid DESC")?;
        statement
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect()
    }
    fn events(&self, id: &str) -> Result<Vec<TaskEvent>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare("SELECT sequence,kind,data FROM events WHERE task_id=?1 ORDER BY sequence")?;
        let rows = statement.query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (sequence, kind, data) = r?;
            Ok(TaskEvent {
                sequence,
                kind,
                data: serde_json::from_str(&data)?,
            })
        })
        .collect()
    }
    fn delete(&self, id: &str) -> Result<()> {
        uuid::Uuid::parse_str(id)?;
        let artifacts = self.root.join(id);
        if artifacts.exists() {
            std::fs::remove_dir_all(artifacts)?;
        }
        let mut connection = self.connection.lock();
        let tx = connection.transaction()?;
        tx.execute("DELETE FROM events WHERE task_id=?1", [id])?;
        tx.execute("DELETE FROM tasks WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    fn save_artifact(&self, id: &str, name: &str, content: &Value) -> Result<()> {
        uuid::Uuid::parse_str(id)?;
        uuid::Uuid::parse_str(name)?;
        let mut redacted = content.clone();
        redact(&mut redacted, &self.secrets.lock());
        dsh_builtin::agent::files::write(
            &self.root.join(id).join(format!("{name}.json")),
            &redacted.to_string(),
        )?;
        Ok(())
    }
    fn load_artifact(&self, id: &str, name: &str) -> Result<Value> {
        uuid::Uuid::parse_str(id)?;
        uuid::Uuid::parse_str(name)?;
        Ok(serde_json::from_str(&dsh_builtin::agent::files::read(
            &self.root.join(id).join(format!("{name}.json")),
        )?)?)
    }
}

/// The fixed `stop_reason` [`SqliteTaskStore::recover_interrupted`] gives a
/// task it just marked `Interrupted` after proving its previous owner is
/// gone. `dsh/src/agent/doctor.rs`'s "crashed" warning matches on this
/// exact text (on the already-loaded `AgentTask`, not a second `events()`
/// fetch) instead of re-deriving the same fact from the `"recovered"` event
/// kind this function also writes.
pub(crate) const RECOVERED_STOP_REASON: &str =
    "previous shell stopped; inspect persisted results before resuming";

/// Default cumulative token budget for a new task when neither `--tokens`
/// nor `AI_AGENT_TOKEN_BUDGET` names one. Mirrors the value the docs use in
/// examples; `cron add --agent` falls back to the same constant
/// (`dsh/src/cron/cli/parse.rs`) so an unattended run starts with the same
/// budget an interactive one would.
pub(crate) const DEFAULT_AGENT_TOKEN_BUDGET: u64 = 50_000;
/// Default cumulative time budget in seconds, same fallback chain as above
/// (`--timeout`, then `AI_AGENT_TIMEOUT_SECS`).
pub(crate) const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 900;

const HELP: &str = "agent run [--tokens N] [--timeout SECONDS] [--check TEXT] [--write DIR] [--read DIR] [--allow-command EXACT] [--profile NAME] [--allow-mcp ENTRY] [--sandbox] [--network HOST] [--env NAME] [--detach|-d] [--dry-run] -- GOAL\nagent resume ID [--tokens N] [--timeout SECONDS] [--reconcile TEXT] [--allow-command EXACT] [--profile NAME] [--detach|-d] [--dry-run]\nagent retry ID [--tokens N] [--timeout SECONDS] [--check TEXT] [--reconcile TEXT] [--allow-command EXACT] [--profile NAME] [--detach|-d] [--dry-run]\nagent profiles\nagent list [--all] [--json] | logs ID [--follow] [--json] | wait ID [--timeout SECONDS]\nagent show ID [--summary] | cancel ID | delete ID | doctor [--json]\nagent respond ID SERVER REMOTE_TASK_ID JSON_INPUT_RESPONSES\n--profile expands to exact commands (see `agent profiles`); resume/retry also accept grant options. --dry-run prints the expanded grant without starting. --detach (-d) starts the task in a separate process and returns immediately; see `agent list`/`agent logs`/`agent wait` to follow it.\nBudgets: --tokens/--timeout, else AI_AGENT_TOKEN_BUDGET / AI_AGENT_TIMEOUT_SECS (shell variable, then environment), else 50000 tokens / 900s. Token budget stops subsequent requests, not a billing cap. AI_AGENT_MAX_CONCURRENT (default 1) bounds how many tasks - detached or not - may run at once.\n";

/// Whether a turn that just ended needs an explicit
/// `AgentLifecycleManager::report_blocked` call rather than letting
/// `TurnGuard::drop` report `Idle` as usual.
///
/// A persistent task's approval requests never reach the interactive
/// `confirm_action` bracket: `dsh/src/proxy/mod.rs`'s `agent_runtime.is_some()`
/// branch sets `TaskStatus::InputRequired` and returns immediately instead of
/// waiting on anyone, since nothing is watching an unattended task. So for
/// this path, "blocked" is a final state the turn ends in - detected here,
/// after `execute_chat_message` has already returned - not a bracket that
/// resolves before the turn is over.
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
        // would duplicate the same work (and burn budget twice when the
        // concurrency ceiling allows it).
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
            token_budget: source.token_budget,
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
            token_budget: setting(shell, "AI_AGENT_TOKEN_BUDGET")
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_AGENT_TOKEN_BUDGET),
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
            "--tokens" => task.token_budget = value.parse()?,
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
            "goal: {}\ncommands: {}\nread: {}\nwrite: {}\ntokens: {}/{}\ntimeout_secs: {}\ndetach: {detach}",
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
            task.token_budget,
            task.time_budget_ms / 1000,
        ))?;
        // Fail fast like `detach::start`'s parent-side check: a preview that
        // passes while the real run would immediately fail (empty goal,
        // exhausted budget, unreconciled operation) is worse than no preview.
        if let Err(error) = validate::startable(shell, &store, &task, reconcile.as_deref()) {
            bail!("dry-run validation failed: {error:#}");
        }
        return Ok(());
    }
    if detach {
        return detach::start(shell, ctx, &store, task, reconcile);
    }
    let _lock = match locks::admit_run(shell, &store, &task.id)? {
        locks::Admission::Admitted(lock) => lock,
        locks::Admission::TaskBusy => bail!("task {} is already running", task.id),
        locks::Admission::NoFreeSlot => {
            bail!("another agent task is active; cancel it, wait, or raise AI_AGENT_MAX_CONCURRENT")
        }
    };
    let report = run_task(shell, ctx, &store, task, reconcile)?;
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
    // `agent_runtime.is_some()` branch sets `TaskStatus::InputRequired` and
    // returns immediately instead of waiting on anyone) - so "blocked" here
    // is a final state the turn ends in, not one that resolves before it
    // does. Reported explicitly, before `turn` drops, so `TurnGuard::drop`
    // sees it and skips forcing `Idle` over it.
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
