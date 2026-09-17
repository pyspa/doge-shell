//! SQLite-backed [`AgentTaskStore`](dsh_builtin::shell_capabilities::AgentTaskStore) and the [`TaskFailure`] markers [`run_task`](super::run_task) tags its errors with.
use super::locks;
use anyhow::{Context as _, Result, bail};
use dsh_builtin::shell_capabilities::{AgentTaskSave, AgentTaskStore};
use dsh_types::agent::{AgentTask, TaskEvent, TaskStatus};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    fs::{DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Why [`run_task`](super::run_task) failed before it could even produce a [`TaskRunReport`](super::TaskRunReport),
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
    /// runtime, an empty goal, or an exhausted time budget - not something a
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
pub(crate) fn tag_failure<E: std::fmt::Display>(failure: TaskFailure, error: E) -> anyhow::Error {
    anyhow::Error::new(failure).context(error.to_string())
}

pub struct SqliteTaskStore {
    connection: Mutex<Connection>,
    pub(crate) root: PathBuf,
    pub(crate) secrets: Mutex<Vec<String>>,
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
    pub(crate) fn cancel(&self, id: &str) -> Result<()> {
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
