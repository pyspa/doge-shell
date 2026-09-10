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
    fs::{DirBuilder, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

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
    fn execution_lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join("active.lock"))?;
        file.try_lock()
            .context("another agent task is active; cancel it or wait")?;
        Ok(file)
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
    fn recover_interrupted(&self) -> Result<()> {
        // A live owner holds the lock. Only recover after proving it is gone.
        let Ok(_lock) = self.execution_lock() else {
            return Ok(());
        };
        for mut task in self.list()? {
            if task.status == TaskStatus::Running {
                let jobs = dsh_builtin::agent::unfinished_local_jobs(&self.events(&task.id)?);
                if !jobs.is_empty() && task.pending_operation.is_none() {
                    task.pending_operation = Some(
                        json!({"unfinished_jobs":jobs,"reason":"previous processes may still be running; inspect their effects before resuming"}),
                    );
                }
                task.status = TaskStatus::Interrupted;
                task.stop_reason = Some(
                    "previous shell stopped; inspect persisted results before resuming".into(),
                );
                self.save(&task, Some(("recovered", &Value::Null)))?;
            }
        }
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

const HELP: &str = "agent run --tokens N --timeout SECONDS [--check TEXT] [--write DIR] [--read DIR] [--allow-command EXACT] [--allow-mcp ENTRY] [--sandbox] [--network HOST] [--env NAME] -- GOAL\nagent resume ID [--tokens N] [--timeout SECONDS] [--reconcile TEXT]\nagent list | show ID | cancel ID | delete ID\nagent respond ID SERVER REMOTE_TASK_ID JSON_INPUT_RESPONSES\nBudgets: AI_AGENT_TOKEN_BUDGET / AI_AGENT_TIMEOUT_SECS (shell variable, then environment). Token budget stops subsequent requests, not a billing cap.\n";

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
fn blocked_reason_for(status: TaskStatus, stop_reason: Option<&str>) -> Option<String> {
    (status == TaskStatus::InputRequired).then(|| {
        stop_reason
            .map(str::to_string)
            .unwrap_or_else(|| "agent task needs approval".to_string())
    })
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
        for task in store.list()? {
            ctx.write_stdout(&format!(
                "{} {:?} {} / {} tokens {}\n",
                task.id, task.status, task.tokens_used, task.token_budget, task.goal
            ))?;
        }
        return Ok(());
    }
    if action == "respond" {
        let id = argv.get(2).context("task ID required")?;
        let server = argv.get(3).context("server label required")?;
        let remote = argv.get(4).context("remote task ID required")?;
        let input: Value =
            serde_json::from_str(argv.get(5).context("JSON inputResponses required")?)?;
        let _lock = store.execution_lock()?;
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
                let _lock = store.execution_lock()?;
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
    if !matches!(action, "run" | "resume") {
        bail!("unknown agent action; {HELP}");
    }
    let _lock = store.execution_lock()?;
    let setting = |shell: &mut crate::shell::Shell, key: &str| {
        shell.get_var(key).or_else(|| std::env::var(key).ok())
    };
    let root = shell.get_current_dir()?.canonicalize()?;
    let mut task = if action == "resume" {
        store.load(argv.get(2).context("task ID required")?)?
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
                .unwrap_or(0),
            tokens_used: 0,
            time_budget_ms: setting(shell, "AI_AGENT_TIMEOUT_SECS")
                .and_then(|s| s.parse::<u64>().ok())
                .and_then(|s| s.checked_mul(1000))
                .unwrap_or(0),
            elapsed_ms: 0,
            stop_reason: None,
            checkpoint: None,
            pending_operation: None,
            created_at: chrono::Utc::now().timestamp(),
        }
    };
    let mut index = if action == "resume" { 3 } else { 2 };
    let mut reconcile = None;
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
        let value = argv.get(index).context("option value required")?;
        index += 1;
        match option.as_str() {
            "--tokens" => task.token_budget = value.parse()?,
            "--timeout" => {
                task.time_budget_ms = value
                    .parse::<u64>()?
                    .checked_mul(1000)
                    .context("timeout too large")?
            }
            "--reconcile" if action == "resume" => reconcile = Some(value.clone()),
            "--read" | "--write" if matches!(action, "run" | "resume") => {
                let path = PathBuf::from(shellexpand::tilde(value).as_ref()).canonicalize()?;
                if !path.is_dir() {
                    bail!("grant must name an existing directory");
                }
                if option == "--read" {
                    task.grant.read_roots.push(path);
                } else {
                    task.grant.write_roots.push(path);
                }
            }
            "--allow-command" if matches!(action, "run" | "resume") => {
                task.grant.commands.push(value.clone())
            }
            "--allow-mcp" if matches!(action, "run" | "resume") => {
                task.grant.mcp_calls.push(value.clone())
            }
            "--network" if matches!(action, "run" | "resume") => {
                if value.contains(['/', ':', '*']) || value.trim().is_empty() {
                    bail!("network grant must be an exact host");
                }
                task.grant.network_hosts.push(value.clone());
            }
            "--env" if matches!(action, "run" | "resume") => {
                task.grant.environment.push(value.clone())
            }
            "--check" if action == "run" => task.criteria.push(Verification {
                criterion: value.clone(),
                evidence_event: None,
                passed: false,
            }),
            _ => bail!("unsupported option {option}"),
        }
    }
    for path in task
        .grant
        .read_roots
        .iter()
        .chain(&task.grant.write_roots)
        .chain(std::iter::once(&task.root))
    {
        if path.canonicalize()? != *path {
            bail!("task root changed identity; inspect and start a new task");
        }
    }
    if task.grant.sandbox {
        dsh_builtin::agent::sandbox::find_runtime()?;
    }
    for name in &task.grant.environment {
        if dsh_types::safety_policy::is_sensitive_key(name)
            && let Some(value) = setting(shell, name)
            && value.len() >= 4
        {
            store.secrets.lock().push(value);
        }
    }
    if task.goal.trim().is_empty() {
        bail!("goal required after --");
    }
    if task.token_budget <= task.tokens_used || task.time_budget_ms <= task.elapsed_ms {
        bail!("positive remaining --tokens and --timeout budgets are required");
    }
    if task.pending_operation.is_some() && reconcile.is_none() {
        bail!(
            "previous operation has an unknown outcome; inspect `agent show {}` and the actual files/service, then resume with --reconcile describing the observed result",
            task.id
        );
    }
    if let Some(note) = reconcile {
        task.pending_operation = None;
        task.progress = format!("User reconciled interrupted operation: {note}");
        store.save(&task, Some(("reconciled", &json!(note))))?;
    }
    // Explicit resume is the only path that may clear a cancellation.
    task.status = TaskStatus::Running;
    task.stop_reason = None;
    task = store.resume(&task, Some(("started", &Value::Null)))?.task;
    let old_cwd = shell.get_current_dir()?;
    ctx.write_stdout(&format!("Task {}\n", task.id))?;
    if let Err(error) = shell.changepwd(&task.root.to_string_lossy()) {
        task.status = TaskStatus::Failed;
        task.stop_reason = Some(error.to_string());
        store.save(&task, None)?;
        let _ = shell.changepwd(&old_cwd.to_string_lossy());
        return Err(error);
    }
    let goal = task.goal.clone();
    let id = task.id.clone();
    shell.agent_runtime = Some(Arc::new(Mutex::new(AgentRuntime::new(task, store))));
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
    let cleanup = if let Some(runtime) = shell.agent_runtime.take() {
        let mut runtime = runtime.lock();
        let result = if runtime.task.status == TaskStatus::Running {
            runtime.finish(false, Some(format!("chat exited: {status:?}")))
        } else {
            Ok(())
        };
        completed = runtime.task.status == TaskStatus::Completed;
        blocked_reason =
            blocked_reason_for(runtime.task.status, runtime.task.stop_reason.as_deref());
        result.and_then(|()| {
            ctx.write_stdout(&format!(
                "Task {id}: {:?} — {}\n",
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
    if status != dsh_types::ExitStatus::ExitedWith(0) || !completed {
        bail!("task {id} stopped; inspect with agent show");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
