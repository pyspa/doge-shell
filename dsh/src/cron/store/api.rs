//! The [`CronStore`] surface, as the `cron` builtin sees it.
//!
//! Everything here is a thin shell over one statement or one transaction; the
//! decisions that are easy to get wrong - claiming, completing, opening an
//! incident - live in `claim.rs`, and the row plumbing in `rows.rs`.
//!
//! Two rules repeat across these methods and are worth reading once:
//! `now` always arrives as an argument rather than being read here, so tests
//! can move years in a line; and pausing clears `next_run_at` while resuming
//! recomputes it, because the point of pausing is not to owe the runs that
//! elapsed meanwhile.

use super::*;

/// Escapes `%`, `_` and the escape character itself, so a caller-supplied
/// run-id prefix is matched literally by `LIKE ?1 || '%' ESCAPE '\'` instead
/// of having any wildcard characters it happens to contain interpreted as
/// LIKE syntax.
fn escape_like_pattern(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl CronStore for SqliteCronStore {
    fn create(
        &self,
        spec: &CronJobSpec,
        env: &HashMap<String, String>,
        now: i64,
        force: bool,
    ) -> Result<i64> {
        // An explicit `--force` re-add is a person's own command; its
        // `--paused`/no-`--paused` spelling takes effect, same as a fresh add.
        self.insert_job(spec, env, now, force, false)
    }

    fn upsert(&self, spec: &CronJobSpec, env: &HashMap<String, String>, now: i64) -> Result<i64> {
        // `config.lisp` re-declaring a job on every launch must not silently
        // undo a `cron pause` the user made since the last one - see
        // `insert_job`'s doc comment.
        self.insert_job(spec, env, now, true, true)
    }

    fn patch(&self, selector: &str, patch: &CronJobPatch, now: i64) -> Result<String> {
        if patch.is_empty() {
            bail!("nothing to change; name at least one field to edit");
        }
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = resolve(&tx, selector)?;

        if let Some(name) = &patch.name {
            tx.execute(
                "UPDATE jobs SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, name, now],
            )?;
        }
        if let Some((schedule, spec)) = &patch.schedule {
            // The next slot has to move with the schedule, or the job keeps
            // one more appointment under the old rule - but only for a job
            // that is actually enabled: a paused job's `next_run_at` must
            // stay `NULL` (the same invariant `insert_job`/`set_paused` keep)
            // or every JSON/tool surface that reads the column raw would show
            // a concrete timestamp for a job that will not fire until it is
            // resumed.
            let enabled: bool =
                tx.query_row("SELECT enabled FROM jobs WHERE id = ?1", [id], |row| {
                    row.get::<_, i64>(0)
                })? != 0;
            let next = enabled
                .then(|| crate::cron::clock::next_run_at(*schedule, now))
                .flatten();
            tx.execute(
                "UPDATE jobs SET schedule_spec = ?2, schedule_kind = ?3, next_run_at = ?4, \
                 updated_at = ?5 WHERE id = ?1",
                params![id, spec, schedule.kind_str(), next, now],
            )?;
        }
        if let Some(command) = &patch.command {
            tx.execute(
                "UPDATE jobs SET command = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, command, now],
            )?;
        }
        if let Some(agent) = &patch.agent {
            tx.execute(
                "UPDATE jobs SET payload = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, serde_json::to_string(agent)?, now],
            )?;
        }
        if let Some(cwd) = &patch.cwd {
            tx.execute(
                "UPDATE jobs SET cwd = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, cwd, now],
            )?;
        }
        if let Some(notify) = patch.notify {
            tx.execute(
                "UPDATE jobs SET notify = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, notify.as_str(), now],
            )?;
        }
        if let Some(timeout) = patch.timeout_secs {
            tx.execute(
                "UPDATE jobs SET timeout_secs = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, timeout as i64, now],
            )?;
        }
        if let Some(catchup) = patch.catchup_secs {
            tx.execute(
                "UPDATE jobs SET catchup_secs = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, catchup as i64, now],
            )?;
        }

        // An interval job that outlives its own interval starves its own
        // next run - `build_spec` enforces this unconditionally at create
        // time, even against an explicit `--timeout` (see
        // `cli/parse.rs::build_spec`), so an edit has to match: applied last,
        // after any `--timeout` this same call also named, so a schedule
        // change alone cannot leave a stale, now-too-long timeout in place,
        // and a schedule change paired with an explicit `--timeout` is
        // clamped exactly the way `cron add` would clamp it.
        if let Some((Schedule::Every(interval), _)) = &patch.schedule {
            let interval_secs = interval.secs() as i64;
            tx.execute(
                "UPDATE jobs SET timeout_secs = MIN(timeout_secs, ?2) WHERE id = ?1",
                params![id, interval_secs],
            )?;
            // An AI job's watchdog is armed from its own stored
            // `time_budget_secs` inside `payload`, not from `jobs.timeout_secs`
            // - `parse_edit` only resyncs the two when `--timeout` or a
            // grant-shaped flag is also named (see its own doc comment on why
            // they must stay in lock-step). A schedule-only edit does
            // neither, so the clamp above has to mirror itself into the
            // payload directly here, or it reopens the exact lease/watchdog
            // drift that resync exists to prevent - just via `--schedule`
            // instead of `--timeout`.
            let payload: Option<String> =
                tx.query_row("SELECT payload FROM jobs WHERE id = ?1", [id], |row| {
                    row.get(0)
                })?;
            if let Some(payload) = payload {
                let mut agent: dsh_types::cron::job::AgentJobSpec = serde_json::from_str(&payload)?;
                if agent.time_budget_secs > interval_secs as u64 {
                    agent.time_budget_secs = interval_secs as u64;
                    tx.execute(
                        "UPDATE jobs SET payload = ?2 WHERE id = ?1",
                        params![id, serde_json::to_string(&agent)?],
                    )?;
                }
            }
        }

        let name = job_name(&tx, id)?;
        tx.commit()?;
        Ok(name)
    }

    fn delete(&self, selector: &str) -> Result<String> {
        let name = {
            // Resolving the selector to an id and deleting it have to be one
            // transaction: two bare statements on the same connection each
            // commit on their own, so another process (this store is shared
            // across a session and an external tick, never just one thread)
            // could delete or rename the row between them.
            let mut connection = self.connection.lock();
            let tx =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let id = resolve(&tx, selector)?;
            let name = job_name(&tx, id)?;
            tx.execute("DELETE FROM jobs WHERE id = ?1", [id])?;
            tx.commit()?;
            name
        };
        // The notepad is the job's memory; it goes with the job.
        std::fs::remove_file(self.notepad_path(&name)).ok();
        std::fs::remove_file(self.lease_path(&name)).ok();
        Ok(name)
    }

    fn get(&self, selector: &str) -> Result<CronJobView> {
        let connection = self.connection.lock();
        let id = resolve(&connection, selector)?;
        let mut job =
            connection.query_row(&format!("{SELECT_JOB} WHERE id = ?1"), [id], job_from_row)?;
        job.last = connection
            .query_row(
                &format!("{SELECT_RUN} WHERE r.job_id = ?1 ORDER BY r.scheduled_for DESC LIMIT 1"),
                [id],
                run_from_row,
            )
            .optional()?;
        Ok(job)
    }

    fn list(&self) -> Result<Vec<CronJobView>> {
        let connection = self.connection.lock();
        let mut latest = self.latest_runs(&connection)?;
        let mut statement = connection.prepare(&format!("{SELECT_JOB} ORDER BY name"))?;
        let rows = statement.query_map([], job_from_row)?;
        let mut jobs = Vec::new();
        for job in rows {
            let mut job = job?;
            job.last = latest.remove(&job.id);
            jobs.push(job);
        }
        Ok(jobs)
    }

    fn set_paused(&self, selector: &str, paused: bool, now: i64) -> Result<String> {
        // One transaction for the same reason as `delete`: resolving the
        // selector and writing the new state are two statements, and only a
        // transaction stops another process from deleting or renaming the
        // row in between.
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = resolve(&tx, selector)?;
        let spec: String = tx.query_row(
            "SELECT schedule_spec FROM jobs WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        let schedule = parse_schedule(&spec).map_err(anyhow::Error::msg)?;
        // Resuming re-bases rather than restoring the old slot: the point of
        // pausing is to not owe the runs that elapsed meanwhile.
        let next = if paused {
            None
        } else {
            crate::cron::clock::next_run_at(schedule, now)
        };
        tx.execute(
            "UPDATE jobs SET enabled = ?2, individually_paused = ?3, next_run_at = ?4, \
             updated_at = ?5 WHERE id = ?1",
            params![id, i64::from(!paused), i64::from(paused), next, now],
        )?;
        let name = job_name(&tx, id)?;
        tx.commit()?;
        Ok(name)
    }

    fn set_all_paused(&self, paused: bool, now: i64) -> Result<usize> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let affected = if paused {
            // The master switch disables everything, individually-paused or
            // not - there is only one way to be off. `individually_paused`
            // is left exactly as it was, so a later global resume still
            // knows which jobs to leave alone.
            tx.execute(
                "UPDATE jobs SET enabled = 0, next_run_at = NULL, updated_at = ?1",
                [now],
            )?
        } else {
            // Only jobs that were not paused on their own come back - a job
            // paused by name (`cron pause <job>`) must stay paused through a
            // global pause/resume cycle, or `cron pause <job>` would mean
            // nothing once anyone ran a global `cron resume`. And only a job
            // that was actually off is rebased: an already-enabled job has a
            // countdown already in progress, and this call must not reset it
            // just because some other job also happened to need resuming.
            let ids: Vec<i64> = {
                let mut statement = tx
                    .prepare("SELECT id FROM jobs WHERE individually_paused = 0 AND enabled = 0")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if !ids.is_empty() {
                tx.execute(
                    "UPDATE jobs SET enabled = 1, updated_at = ?1 \
                     WHERE individually_paused = 0 AND enabled = 0",
                    [now],
                )?;
                self.rebase_ids(&tx, &ids, now)?;
            }
            ids.len()
        };
        tx.commit()?;
        Ok(affected)
    }

    fn trigger(&self, selector: &str, now: i64) -> Result<String> {
        // Same reason as `delete` and `set_paused`: resolve-then-mutate is
        // two statements, and only a transaction keeps another process from
        // deleting or renaming the row in between.
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = resolve(&tx, selector)?;
        tx.execute(
            "UPDATE jobs SET next_run_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        let name = job_name(&tx, id)?;
        tx.commit()?;
        Ok(name)
    }

    fn claim_due(
        &self,
        now: i64,
        owner: &str,
        limit: usize,
        trigger: RunTrigger,
    ) -> Result<Vec<ClaimedRun>> {
        self.claim_due_jobs(now, owner, limit, trigger)
    }

    fn claim_one(
        &self,
        selector: &str,
        now: i64,
        owner: &str,
        trigger: RunTrigger,
    ) -> Result<ClaimedRun> {
        self.claim_one_job(selector, now, owner, trigger)
    }

    fn start(&self, run_id: &str, now: i64) -> Result<ClaimedRun> {
        self.start_run(run_id, now)
    }

    fn attach_agent_task(&self, run_id: &str, task_id: &str) -> Result<()> {
        let connection = self.connection.lock();
        let updated = connection.execute(
            "UPDATE runs SET agent_task_id = ?2 WHERE id = ?1",
            params![run_id, task_id],
        )?;
        // No realistic caller mismatches `run_id` today - it is the same id
        // `start` just returned in the same call chain - but silently
        // no-op'ing on a mismatch would be exactly the wrong failure mode:
        // it would look identical to success while quietly losing the one
        // thing that makes a watchdog-killed run findable again.
        if updated == 0 {
            bail!("{run_id}: no such run (agent_task_id was not recorded)");
        }
        Ok(())
    }

    fn complete(&self, run_id: &str, outcome: &RunOutcome, now: i64) -> Result<()> {
        self.complete_run(run_id, outcome, now)
    }

    fn runs(&self, query: &RunQuery) -> Result<Vec<CronRun>> {
        let connection = self.connection.lock();
        let job_id = query
            .job
            .as_deref()
            .map(|selector| resolve(&connection, selector))
            .transpose()?;

        let mut sql = SELECT_RUN.to_string();
        let mut clauses = Vec::new();
        if job_id.is_some() {
            clauses.push("r.job_id = ?1".to_string());
        }
        if query.finished_only {
            clauses.push("r.finished_at IS NOT NULL".to_string());
        }
        if query.failed_only {
            clauses.push("r.state = 'failed'".to_string());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY r.scheduled_for DESC LIMIT ?2");

        let limit = if query.limit == 0 { 20 } else { query.limit } as i64;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![job_id.unwrap_or_default(), limit], run_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn incidents(&self, open_only: bool, limit: usize) -> Result<Vec<CronIncident>> {
        let connection = self.connection.lock();
        let sql = format!(
            "{SELECT_INCIDENT}{} ORDER BY i.opened_at DESC LIMIT ?1",
            if open_only {
                " WHERE i.acked_at IS NULL"
            } else {
                ""
            }
        );
        let limit = if limit == 0 { 20 } else { limit } as i64;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([limit], incident_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn open_incident(
        &self,
        job_id: Option<i64>,
        kind: IncidentKind,
        detail: &str,
        agent_task_id: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        self.record_incident(job_id, kind, detail, agent_task_id, now)
    }

    fn ack_incident(&self, id: i64, now: i64) -> Result<CronIncident> {
        self.acknowledge_incident(id, now)
    }

    fn notepad(&self, selector: &str) -> Result<String> {
        let name = {
            let connection = self.connection.lock();
            let id = resolve(&connection, selector)?;
            job_name(&connection, id)?
        };
        match std::fs::read_to_string(self.notepad_path(&name)) {
            Ok(body) => Ok(body),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn set_notepad(&self, selector: &str, body: &str) -> Result<()> {
        let name = {
            let connection = self.connection.lock();
            let id = resolve(&connection, selector)?;
            job_name(&connection, id)?
        };
        let path = self.notepad_path(&name);
        if let Some(parent) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .ok();
        }
        let trimmed = if body.len() > MAX_NOTEPAD_BYTES {
            &body[..body.floor_char_boundary(MAX_NOTEPAD_BYTES)]
        } else {
            body
        };
        std::fs::write(&path, trimmed)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn reap_expired_leases(&self, now: i64) -> Result<usize> {
        self.reap_leases(now)
    }

    fn health(&self, now: i64) -> Result<CronHealth> {
        let connection = self.connection.lock();
        let (total, running, failing, paused, blocked, overdue): (i64, i64, i64, i64, i64, i64) =
            connection.query_row(
                "SELECT COUNT(*), \
                 SUM(claimed_by IS NOT NULL), \
                 SUM(consecutive_failures > 0), \
                 SUM(enabled = 0), \
                 SUM(blocked != 0), \
                 SUM(enabled = 1 AND blocked = 0 AND next_run_at IS NOT NULL AND next_run_at <= ?1) \
                 FROM jobs",
                [now],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(2)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(3)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(5)?.unwrap_or_default(),
                    ))
                },
            )?;
        let open_incidents: i64 = connection.query_row(
            "SELECT COUNT(*) FROM incidents WHERE acked_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        let last_run_at: Option<i64> =
            connection.query_row("SELECT MAX(finished_at) FROM runs", [], |row| row.get(0))?;
        Ok(CronHealth {
            total: total as usize,
            running: running as usize,
            failing: failing as usize,
            paused: paused as usize,
            blocked: blocked as usize,
            open_incidents: open_incidents as usize,
            overdue: overdue as usize,
            last_run_at,
        })
    }

    fn tokens_used_since(&self, job_id: i64, since: i64) -> Result<u64> {
        let connection = self.connection.lock();
        let total: Option<i64> = connection.query_row(
            "SELECT SUM(tokens_used) FROM runs WHERE job_id = ?1 AND finished_at >= ?2",
            params![job_id, since],
            |row| row.get(0),
        )?;
        Ok(total.unwrap_or_default().max(0) as u64)
    }

    fn next_due_at(&self) -> Result<Option<i64>> {
        let connection = self.connection.lock();
        Ok(connection.query_row(
            "SELECT MIN(next_run_at) FROM jobs \
             WHERE enabled = 1 AND blocked = 0 AND next_run_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?)
    }

    fn run_output(&self, selector: &RunSelector) -> Result<RunOutput> {
        let mut connection = self.connection.lock();
        // In one transaction, not just one held lock: the lock only rules
        // out another thread *in this process* racing the two SELECTs below;
        // a `cron tick`/`cron run-job` running as its own separate process
        // opens its own connection to the same file and is not blocked by
        // it. A transaction is what actually keeps "resolve which run" and
        // "read that run's streams" atomic against a concurrent prune/delete
        // from such a process.
        let tx = connection.transaction()?;
        // Two queries on purpose, not a wider `SELECT_RUN`: that constant's
        // column list and `run_from_row`'s `row.get(n)` calls are one unit
        // (see `rows.rs`'s own module doc), and `stdout`/`stderr` are read by
        // exactly one caller. Widening the shared query would make every
        // `cron list`/`cron history` scan carry two 8 KiB columns it never
        // uses.
        let run = match selector {
            RunSelector::Latest(job) => {
                let id = resolve(&tx, job)?;
                tx.query_row(
                    &format!(
                        "{SELECT_RUN} WHERE r.job_id = ?1 AND r.finished_at IS NOT NULL \
                             ORDER BY r.scheduled_for DESC LIMIT 1"
                    ),
                    [id],
                    run_from_row,
                )
                .optional()?
                .with_context(|| format!("{job}: no finished run recorded yet"))?
            }
            RunSelector::Id(prefix) => {
                // `prefix` is a caller-supplied string (from `--run` or the
                // `cron_manage` tool), not a trusted id - without escaping,
                // a literal `%`/`_` in it would be read as a LIKE wildcard
                // instead of a literal character, letting e.g. `--run '%'`
                // match every run in the store.
                let mut statement = tx.prepare(&format!(
                    "{SELECT_RUN} WHERE r.id LIKE ?1 || '%' ESCAPE '\\' LIMIT 2"
                ))?;
                let mut rows = statement
                    .query_map([escape_like_pattern(prefix)], run_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                match rows.len() {
                    0 => bail!("{prefix}: no run with this id"),
                    1 => rows.remove(0),
                    _ => bail!("{prefix}: matches more than one run; use a longer id"),
                }
            }
            // Both a job selector and a run id/prefix - unlike `Id` alone,
            // the run must actually belong to that job. Without the
            // `r.job_id = ?1` filter, a run id (or prefix) belonging to a
            // *different* job than the one named would still resolve, and
            // the named job would be silently ignored rather than the
            // mismatch being reported.
            RunSelector::JobAndId { job, run } => {
                let id = resolve(&tx, job)?;
                let mut statement = tx.prepare(&format!(
                    "{SELECT_RUN} WHERE r.job_id = ?1 AND r.id LIKE ?2 || '%' ESCAPE '\\' LIMIT 2"
                ))?;
                let mut rows = statement
                    .query_map(params![id, escape_like_pattern(run)], run_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                match rows.len() {
                    0 => bail!("{run}: no run with this id under job `{job}`"),
                    1 => rows.remove(0),
                    _ => {
                        bail!("{run}: matches more than one run under job `{job}`; use a longer id")
                    }
                }
            }
        };
        let (stdout, stderr): (Option<String>, Option<String>) = tx.query_row(
            "SELECT stdout, stderr FROM runs WHERE id = ?1",
            [&run.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.commit()?;
        Ok(RunOutput {
            run,
            stdout: stdout.unwrap_or_default(),
            stderr: stderr.unwrap_or_default(),
        })
    }
}
