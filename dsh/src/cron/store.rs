//! The cron store: jobs, runs, incidents and notepads on SQLite.
//!
//! # Why this is not the agent store
//!
//! `dsh/src/agent.rs` keeps one task at a time behind a `flock` and writes with
//! `journal_mode=DELETE; synchronous=FULL` so a cancelled task's secrets really
//! leave the disk. Cron wants the opposite shape: several processes writing at
//! once, each claiming a different row. That needs WAL, and WAL and "erase it
//! completely" do not go together. Two databases, two sets of trade-offs.
//!
//! # Why the rows are columns, not JSON
//!
//! The agent store puts a whole task in one `TEXT` column because nothing ever
//! queries inside it. Cron's hot path is the opposite - "which rows are due" -
//! and runs every minute from an external tick. That wants an index, so the
//! schedule, the state and the timestamps are real columns. Only the agent
//! grant, which is genuinely nested and never filtered on, stays JSON.
//!
//! # Migrations
//!
//! [`MIGRATIONS`] is append-only and `PRAGMA user_version` records how far a
//! database has been taken. A store written by a **newer** dsh is refused
//! rather than opened: with an external tick in the system crontab, an old
//! binary can easily outlive an upgrade, and silently writing the old shape
//! into a new schema is worse than not running.

mod api;
mod claim;
mod rows;

use rows::*;

#[cfg(test)]
mod tests;

use anyhow::{Context as _, Result, bail};
use dsh_builtin::shell_capabilities::CronStore;
use dsh_types::cron::job::{
    ClaimedRun, CronHealth, CronIncident, CronJobPatch, CronJobSpec, CronJobView, CronRun,
    IncidentKind, JobKind, RunOutcome, RunQuery, RunReason, RunState, RunTrigger,
};
use dsh_types::schedule::{NotifyPolicy, Schedule, parse_schedule};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::HashMap;
use std::fs::{DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Longest a notepad may grow before a run starts trimming it.
///
/// The notepad is prepended to every run's goal, so an unbounded one is a
/// quietly growing token bill on a schedule nobody is watching.
pub const MAX_NOTEPAD_BYTES: usize = 32 * 1024;

/// Runs kept per job before the oldest are pruned.
const RUN_HISTORY_LIMIT: i64 = 200;
/// And a hard age limit, so a job that ran once a year ago does not keep it.
const RUN_HISTORY_MAX_AGE_SECS: i64 = 30 * 24 * 3600;
/// Pruning is not worth a query per run; do it on a fraction of them.
const PRUNE_EVERY: u64 = 50;

const SCHEMA_V1: &str = "
CREATE TABLE jobs(
  id                   INTEGER PRIMARY KEY AUTOINCREMENT,
  name                 TEXT    NOT NULL UNIQUE,
  kind                 TEXT    NOT NULL,
  schedule_spec        TEXT    NOT NULL,
  schedule_kind        TEXT    NOT NULL,
  command              TEXT    NOT NULL,
  payload              TEXT,
  cwd                  TEXT    NOT NULL,
  env                  TEXT    NOT NULL,
  notify               TEXT    NOT NULL,
  timeout_secs         INTEGER NOT NULL,
  catchup_secs         INTEGER NOT NULL,
  enabled              INTEGER NOT NULL DEFAULT 1,
  blocked              INTEGER NOT NULL DEFAULT 0,
  next_run_at          INTEGER,
  last_digest          INTEGER,
  run_count            INTEGER NOT NULL DEFAULT 0,
  fail_count           INTEGER NOT NULL DEFAULT 0,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  consecutive_skips    INTEGER NOT NULL DEFAULT 0,
  claimed_by           TEXT,
  claimed_until        INTEGER,
  created_at           INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL);
CREATE INDEX jobs_due ON jobs(enabled, next_run_at);

CREATE TABLE runs(
  id             TEXT    PRIMARY KEY,
  job_id         INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  scheduled_for  INTEGER NOT NULL,
  state          TEXT    NOT NULL,
  reason         TEXT,
  started_at     INTEGER,
  finished_at    INTEGER,
  duration_ms    INTEGER NOT NULL DEFAULT 0,
  exit_code      INTEGER NOT NULL DEFAULT 0,
  timed_out      INTEGER NOT NULL DEFAULT 0,
  changed        INTEGER NOT NULL DEFAULT 0,
  trigger        TEXT    NOT NULL,
  owner          TEXT    NOT NULL,
  agent_task_id  TEXT,
  tokens_used    INTEGER NOT NULL DEFAULT 0,
  pending_skills INTEGER NOT NULL DEFAULT 0,
  preview        TEXT    NOT NULL DEFAULT '',
  stdout         TEXT,
  stderr         TEXT,
  UNIQUE(job_id, scheduled_for));
CREATE INDEX runs_by_job ON runs(job_id, scheduled_for DESC);

CREATE TABLE incidents(
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id        INTEGER REFERENCES jobs(id) ON DELETE CASCADE,
  opened_at     INTEGER NOT NULL,
  acked_at      INTEGER,
  kind          TEXT    NOT NULL,
  detail        TEXT    NOT NULL,
  agent_task_id TEXT);
CREATE INDEX incidents_open ON incidents(acked_at, opened_at DESC);
";

/// Distinguishes a job paused on its own (`cron pause <job>`) from one only
/// disabled by the global master switch (`cron pause` with no arguments).
/// Without this, both states shared the one `enabled` column, so a global
/// `cron resume` could not tell them apart and silently re-enabled a job the
/// user had individually paused.
const SCHEMA_V2: &str =
    "ALTER TABLE jobs ADD COLUMN individually_paused INTEGER NOT NULL DEFAULT 0;";

/// Append only. The index of a statement is the `user_version` it produces.
const MIGRATIONS: &[&str] = &[SCHEMA_V1, SCHEMA_V2];

#[derive(Debug)]
pub struct SqliteCronStore {
    connection: Mutex<Connection>,
    root: PathBuf,
}

impl SqliteCronStore {
    /// Opens (creating if needed) the store under `root`.
    ///
    /// The permission checks mirror the agent store's: a private directory and
    /// a private file, opened with `O_NOFOLLOW` so a symlink planted in place
    /// of the database cannot redirect the write.
    pub fn open(root: &Path) -> Result<Self> {
        if let Err(error) = DirBuilder::new().recursive(true).mode(0o700).create(root)
            && error.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(error)
                .with_context(|| format!("cannot create cron state directory {}", root.display()));
        }
        let meta = std::fs::symlink_metadata(root)
            .with_context(|| format!("cron state directory {} is unreadable", root.display()))?;
        if meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "cron state directory {} must be a private directory (0700)",
                root.display()
            );
        }

        let path = root.join("jobs.sqlite3");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            bail!("cron database {} must be private (0600)", path.display());
        }
        drop(file);

        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA secure_delete=ON;",
        )?;
        migrate(&mut connection)?;

        Ok(Self {
            connection: Mutex::new(connection),
            root: root.to_path_buf(),
        })
    }

    /// Where a job's notepad lives. Under the cron state directory, never the
    /// agent one: `SafetyGuard::task_file_allowed` refuses that whole subtree,
    /// and the notepad has to be writable by the agent task it belongs to.
    pub fn notepad_path(&self, job: &str) -> PathBuf {
        self.root.join("notepad").join(format!("{}.md", slug(job)))
    }

    /// Where a job's single-run lease lives.
    pub fn lease_path(&self, job: &str) -> PathBuf {
        self.root.join("leases").join(format!("{}.lock", slug(job)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let current = usize::try_from(current).unwrap_or(usize::MAX);
    if current > MIGRATIONS.len() {
        bail!(
            "cron store is at schema {current} but this dsh only knows {}; \
             upgrade dsh, or move the store aside to start over",
            MIGRATIONS.len()
        );
    }
    for (index, statements) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = connection.transaction()?;
        tx.execute_batch(statements)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// Reduces a job name to something safe for a filename.
///
/// Job names are already restricted at the CLI, so this is a second line
/// rather than the only one - but a notepad path is handed to an agent as a
/// write grant, and a name that escaped its directory would widen that grant.
fn slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "job".to_string()
    } else {
        cleaned
    }
}

impl SqliteCronStore {
    /// Inserts a new job, or - when `replace` is set - updates one of the same
    /// name in place.
    ///
    /// "In place" is load-bearing: an upsert exists so `(cron-add ...)` is
    /// safe to leave in `config.lisp`, which runs it again on every launch.
    /// An earlier version used `INSERT OR REPLACE`, which SQLite executes as
    /// a delete-then-insert - with `runs`/`incidents` cascading on
    /// `ON DELETE CASCADE`, that silently erased a job's entire run history
    /// and reset its failure-streak counters on every single relaunch, and
    /// handed it a new id each time. `ON CONFLICT(name) DO UPDATE` updates
    /// the existing row instead: same id, and every column this statement
    /// does not name - `run_count`, `fail_count`, `consecutive_failures`,
    /// `consecutive_skips`, `last_digest`, `claimed_by`, `claimed_until`,
    /// `created_at` - is left exactly as it was.
    ///
    /// `preserve_run_state` keeps `enabled`/`next_run_at`/`individually_paused`
    /// out of that update too, when set. `config.lisp`'s `(cron-add ...)`
    /// (`upsert`, which always passes `true` here) has no way to say "and
    /// leave whatever pause state this job is already in alone" - its spec's
    /// `paused` field is always `false`, simply because `CronJobSpec` has to
    /// carry *some* value. Without this, every single relaunch (every
    /// `config.lisp` re-run) would silently resume a job the user had paused
    /// with `cron pause`, individually or globally - the exact "run it again
    /// on every launch" property this function exists for would quietly undo
    /// a person's own pause. `cron add --force` (`create`, which passes
    /// `false` here) keeps the older, more literal meaning: a person who
    /// explicitly re-declared a job most likely meant its `--paused`/no
    /// `--paused` spelling to take effect.
    fn insert_job(
        &self,
        spec: &CronJobSpec,
        env: &HashMap<String, String>,
        now: i64,
        replace: bool,
        preserve_run_state: bool,
    ) -> Result<i64> {
        let payload = spec.agent.as_ref().map(serde_json::to_string).transpose()?;
        let env_json = serde_json::to_string(env)?;
        let next_run_at = if spec.paused {
            None
        } else {
            super::clock::next_run_at(spec.schedule, now)
        };
        let on_conflict = if replace {
            let run_state_columns = if preserve_run_state {
                ""
            } else {
                ", enabled=excluded.enabled, next_run_at=excluded.next_run_at, \
                 individually_paused=excluded.individually_paused"
            };
            format!(
                "ON CONFLICT(name) DO UPDATE SET \
                 kind=excluded.kind, schedule_spec=excluded.schedule_spec, \
                 schedule_kind=excluded.schedule_kind, command=excluded.command, \
                 payload=excluded.payload, cwd=excluded.cwd, env=excluded.env, \
                 notify=excluded.notify, timeout_secs=excluded.timeout_secs, \
                 catchup_secs=excluded.catchup_secs{run_state_columns}, \
                 updated_at=excluded.updated_at"
            )
        } else {
            String::new()
        };
        let connection = self.connection.lock();
        connection
            .execute(
                &format!(
                    "INSERT INTO jobs(name, kind, schedule_spec, schedule_kind, command, payload, \
                     cwd, env, notify, timeout_secs, catchup_secs, enabled, next_run_at, \
                     individually_paused, created_at, updated_at) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15) \
                     {on_conflict}"
                ),
                params![
                    spec.name,
                    spec.kind.as_str(),
                    spec.schedule_spec,
                    spec.schedule.kind_str(),
                    spec.command,
                    payload,
                    spec.cwd,
                    env_json,
                    spec.notify.as_str(),
                    spec.timeout_secs as i64,
                    spec.catchup_secs as i64,
                    i64::from(!spec.paused),
                    next_run_at,
                    // `--paused` at creation is a decision about *this job*,
                    // the same as `cron pause <job>` - not the master switch.
                    // Left at the column default, a later global `cron resume`
                    // would read it as "off only because everything is off"
                    // and start it, which is exactly what `cron_manage`'s
                    // always-create-paused rule exists to prevent: one
                    // ordinary `cron resume` would arm every job an agent had
                    // created, with nobody having reviewed any of them.
                    i64::from(spec.paused),
                    now,
                ],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    anyhow::anyhow!(
                        "{}: a cron job with that name already exists; pass --force to replace it",
                        spec.name
                    )
                }
                other => anyhow::Error::new(other),
            })?;
        // `INSERT ... ON CONFLICT DO UPDATE` does not touch `last_insert_rowid`
        // on the update branch the way a plain `INSERT` does, so the id has to
        // be looked up rather than assumed.
        Ok(
            connection.query_row("SELECT id FROM jobs WHERE name = ?1", [&spec.name], |row| {
                row.get(0)
            })?,
        )
    }
}

impl SqliteCronStore {
    fn latest_runs(&self, connection: &Connection) -> Result<HashMap<i64, CronRun>> {
        let mut statement = connection.prepare(&format!(
            "{SELECT_RUN} JOIN (SELECT job_id, MAX(finished_at) AS newest FROM runs \
             WHERE finished_at IS NOT NULL GROUP BY job_id) latest \
             ON latest.job_id = r.job_id AND latest.newest = r.finished_at"
        ))?;
        let rows = statement.query_map([], run_from_row)?;
        let mut by_job = HashMap::new();
        for run in rows {
            let run = run?;
            by_job.insert(run.job_id, run);
        }
        Ok(by_job)
    }

    /// Re-bases the named jobs' wall-clock schedules onto `now`.
    ///
    /// Used when some of them go from paused to resumed. Without it, a day
    /// paused is a day of slots that all became due at once the moment a job
    /// came back. Deliberately scoped to `ids` rather than every enabled job:
    /// a job that was never paused has a countdown already in progress, and
    /// rebasing it too - just because some *other* job in the same
    /// `cron resume` call needed it - would silently reset (and typically
    /// delay) a run nobody asked to change.
    ///
    /// A single job with an unparsable `schedule_spec` skips its own rebase
    /// (logging why) rather than aborting the loop - every job after it would
    /// otherwise be left with `next_run_at` still `NULL` forever, for a
    /// problem that was never theirs.
    fn rebase_ids(&self, connection: &Connection, ids: &[i64], now: i64) -> Result<()> {
        let jobs: Vec<(i64, String)> = {
            let mut statement =
                connection.prepare("SELECT id, schedule_spec FROM jobs WHERE id = ?1")?;
            let mut jobs = Vec::with_capacity(ids.len());
            for &id in ids {
                let mut rows = statement.query([id])?;
                if let Some(row) = rows.next()? {
                    jobs.push((row.get(0)?, row.get(1)?));
                }
            }
            jobs
        };
        for (id, spec) in jobs {
            let schedule = match parse_schedule(&spec) {
                Ok(schedule) => schedule,
                Err(error) => {
                    tracing::warn!(
                        "cron: job {id} has an unparsable schedule {spec:?}; \
                         leaving its next run time as-is: {error}"
                    );
                    continue;
                }
            };
            let next = super::clock::next_run_at(schedule, now);
            connection.execute(
                "UPDATE jobs SET next_run_at = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, next, now],
            )?;
        }
        Ok(())
    }
}
