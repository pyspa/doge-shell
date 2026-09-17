//! Taking ownership of a due job, and closing the run it produced.
//!
//! This is the file that makes two drivers safe. Several shells and an
//! external tick all ask "what is due" at once; exactly one of them may get
//! each answer. The guarantee is not a mutex - those processes share nothing -
//! but a conditional `UPDATE` inside an immediate transaction, plus a unique
//! key on `(job_id, scheduled_for)` as a second, independent net.
//!
//! Two rules are easy to lose and expensive to lose:
//!
//! 1. **The claim advances `next_run_at` in the same transaction.** A job that
//!    is claimed and then skipped must still move forward, or it stays due and
//!    every scan claims it again.
//! 2. **A lease expires.** A process that dies mid-run cannot release its own
//!    claim, so the claim has a deadline and the next scan takes it back.

use super::*;

/// Re-exported so the run deadline (`dsh/src/cron/run_job.rs`) fires from
/// the exact same formula the lease expiry below uses, rather than a second
/// constant that could drift from it.
use dsh_types::cron::job::lease_secs;

/// How many consecutive failures or skips before it stops being noise.
const STREAK_TO_INCIDENT: i64 = 3;

/// Which incident, if any, a finished run calls for.
///
/// Only outcomes that will answer the same way next time earn one. A network
/// blip is a failed run; no API key is an incident, because every later tick
/// would reproduce it and bury the first report under identical copies.
fn incident_for(state: RunState, reason: Option<RunReason>) -> Option<IncidentKind> {
    match state {
        RunState::NeedsApproval => Some(IncidentKind::Approval),
        RunState::Failed => match reason {
            Some(RunReason::Config) => Some(IncidentKind::Config),
            Some(RunReason::Provider) => Some(IncidentKind::Provider),
            Some(RunReason::HookDeny) => Some(IncidentKind::HookAsk),
            Some(RunReason::Reconcile) => Some(IncidentKind::Reconcile),
            Some(RunReason::RootChanged) => Some(IncidentKind::RootChanged),
            Some(RunReason::StateUnusable) => Some(IncidentKind::StateUnusable),
            _ => None,
        },
        RunState::Skipped => match reason {
            Some(RunReason::BudgetExhausted) => Some(IncidentKind::Budget),
            _ => None,
        },
        _ => None,
    }
}

fn claimed_run_from_row(
    row: &Row<'_>,
    trigger: RunTrigger,
    run_id: String,
) -> rusqlite::Result<ClaimedRun> {
    let kind: String = row.get(2)?;
    let payload: Option<String> = row.get(5)?;
    let notify: String = row.get(8)?;
    let env: String = row.get(7)?;
    Ok(ClaimedRun {
        run_id,
        job_id: row.get(0)?,
        job_name: row.get(1)?,
        kind: JobKind::parse(&kind).map_err(to_sql_error)?,
        command: row.get(4)?,
        agent: decode_payload(payload.as_deref()).map_err(to_sql_error)?,
        cwd: row.get(6)?,
        env: serde_json::from_str(&env)
            .map_err(|error| to_sql_error(format!("job environment is unreadable: {error}")))?,
        timeout_secs: row.get::<_, i64>(9)? as u64,
        notify: NotifyPolicy::parse(&notify).map_err(to_sql_error)?,
        scheduled_for: 0,
        trigger,
        last_digest: row.get::<_, Option<i64>>(14)?.map(|value| value as u64),
    })
}

impl SqliteCronStore {
    pub(super) fn claim_due_jobs(
        &self,
        now: i64,
        owner: &str,
        limit: usize,
        trigger: RunTrigger,
    ) -> Result<Vec<ClaimedRun>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // A dead owner cannot release its own claim, so every scan starts by
        // taking back the ones whose deadline has passed.
        tx.execute(
            "UPDATE jobs SET claimed_by = NULL, claimed_until = NULL \
             WHERE claimed_until IS NOT NULL AND claimed_until <= ?1",
            [now],
        )?;

        let due: Vec<(i64, i64, u64, u64, Schedule)> = {
            let mut statement = tx.prepare(
                "SELECT id, next_run_at, timeout_secs, catchup_secs, schedule_spec FROM jobs \
                 WHERE enabled = 1 AND blocked = 0 AND next_run_at IS NOT NULL \
                   AND next_run_at <= ?1 AND claimed_by IS NULL \
                 ORDER BY next_run_at LIMIT ?2",
            )?;
            let rows = statement.query_map(params![now, limit as i64], |row| {
                let spec: String = row.get(4)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                    parse_schedule(&spec).map_err(to_sql_error)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut claimed = Vec::new();
        for (id, scheduled_for, timeout_secs, catchup_secs, schedule) in due {
            let next =
                crate::cron::clock::advance(schedule, scheduled_for, now, catchup_secs as i64);
            let changed = tx.execute(
                "UPDATE jobs SET claimed_by = ?1, claimed_until = ?2, next_run_at = ?3, \
                 updated_at = ?4 WHERE id = ?5 AND claimed_by IS NULL",
                // `saturating_add`, not `+`: `lease_secs` can return `i64::MAX`
                // for a validated `timeout_secs` right at the edge of what
                // `parse_named_duration` allows, and a plain `+` with `now`
                // would overflow - panicking in a debug build, or silently
                // wrapping to a negative `claimed_until` in release, which
                // the very next scan would treat as already-expired and reap
                // out from under a run that is still legitimately going.
                params![
                    owner,
                    now.saturating_add(lease_secs(timeout_secs)),
                    next,
                    now,
                    id
                ],
            )?;
            if changed == 0 {
                // Another driver took it between the select and here.
                continue;
            }

            let run_id = uuid::Uuid::new_v4().to_string();
            // The unique key is the second net: when a fall-back repeats an
            // hour, the same slot cannot produce a second run.
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO runs(id, job_id, scheduled_for, state, trigger, owner) \
                 VALUES(?1, ?2, ?3, 'queued', ?4, ?5)",
                params![run_id, id, scheduled_for, trigger.as_str(), owner],
            )?;
            if inserted == 0 {
                tx.execute(
                    "UPDATE jobs SET claimed_by = NULL, claimed_until = NULL WHERE id = ?1",
                    [id],
                )?;
                continue;
            }

            let mut run = tx.query_row(&format!("{SELECT_JOB} WHERE id = ?1"), [id], |row| {
                claimed_run_from_row(row, trigger, run_id.clone())
            })?;
            run.scheduled_for = scheduled_for;
            claimed.push(run);
        }

        tx.commit()?;
        Ok(claimed)
    }

    /// Claims exactly the named job, ignoring `next_run_at` and `blocked`,
    /// but not an existing claim — two overlapping manual runs of the same
    /// job are still refused, the same as an ordinary due-scan collision.
    pub(super) fn claim_one_job(
        &self,
        selector: &str,
        now: i64,
        owner: &str,
        trigger: RunTrigger,
    ) -> Result<ClaimedRun> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = resolve(&tx, selector)?;

        let (timeout_secs, schedule_spec): (u64, String) = tx.query_row(
            "SELECT timeout_secs, schedule_spec FROM jobs WHERE id = ?1",
            [id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)),
        )?;
        let schedule = parse_schedule(&schedule_spec).map_err(anyhow::Error::msg)?;
        let scheduled_for = now;

        let changed = tx.execute(
            "UPDATE jobs SET claimed_by = ?1, claimed_until = ?2, updated_at = ?3              WHERE id = ?4 AND claimed_by IS NULL",
            // `saturating_add` - see the identical comment in `claim_due_jobs`.
            params![owner, now.saturating_add(lease_secs(timeout_secs)), now, id],
        )?;
        if changed == 0 {
            bail!("{selector}: already running; wait for it to finish or check `cron history`");
        }

        let run_id = uuid::Uuid::new_v4().to_string();
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO runs(id, job_id, scheduled_for, state, trigger, owner)              VALUES(?1, ?2, ?3, 'queued', ?4, ?5)",
            params![run_id, id, scheduled_for, trigger.as_str(), owner],
        )?;
        if inserted == 0 {
            tx.execute(
                "UPDATE jobs SET claimed_by = NULL, claimed_until = NULL WHERE id = ?1",
                [id],
            )?;
            bail!("{selector}: a run for this instant is already recorded; try again in a moment");
        }

        let mut run = tx.query_row(&format!("{SELECT_JOB} WHERE id = ?1"), [id], |row| {
            claimed_run_from_row(row, trigger, run_id.clone())
        })?;
        run.scheduled_for = scheduled_for;
        // A manual claim does not consult the schedule for `next_run_at` - it
        // is not the job's regular slot - but skipping the advance would let
        // this run collide with the very next automatic one at the same
        // instant. Advancing from `now` keeps them apart.
        let next = super::super::clock::next_run_at(schedule, now);
        tx.execute(
            "UPDATE jobs SET next_run_at = COALESCE(?2, next_run_at) WHERE id = ?1",
            params![id, next],
        )?;
        tx.commit()?;
        Ok(run)
    }

    pub(super) fn start_run(&self, run_id: &str, now: i64) -> Result<ClaimedRun> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let started = tx.execute(
            "UPDATE runs SET state = 'running', started_at = ?2 WHERE id = ?1 AND state = 'queued'",
            params![run_id, now],
        )?;
        if started == 0 {
            bail!("run {run_id} is not waiting to start; it may already have run");
        }
        let (job_id, scheduled_for, trigger): (i64, i64, String) = tx.query_row(
            "SELECT job_id, scheduled_for, trigger FROM runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let trigger = RunTrigger::parse(&trigger).map_err(anyhow::Error::msg)?;
        let mut run = tx.query_row(&format!("{SELECT_JOB} WHERE id = ?1"), [job_id], |row| {
            claimed_run_from_row(row, trigger, run_id.to_string())
        })?;
        run.scheduled_for = scheduled_for;
        tx.commit()?;
        Ok(run)
    }

    pub(super) fn complete_run(&self, run_id: &str, outcome: &RunOutcome, now: i64) -> Result<()> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let (job_id, previous_digest): (i64, Option<i64>) = tx.query_row(
            "SELECT r.job_id, j.last_digest FROM runs r JOIN jobs j ON j.id = r.job_id \
             WHERE r.id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        // Only the store may decide "changed": it owns both sides of the
        // comparison, so a caller cannot claim a change without also moving
        // what the next run is compared against.
        //
        // A first run is never "changed" - it has nothing to differ from, and
        // treating it as a change would make every job announce itself once on
        // the `--on change` policy. This matches what `sched` did.
        let changed = match (previous_digest, outcome.digest) {
            (Some(previous), Some(current)) => previous != current as i64,
            _ => false,
        };

        // `agent_task_id` is `COALESCE`d, not overwritten outright:
        // `attach_agent_task` may already have recorded it when the run
        // started, and an early-failure outcome (config/budget/lock, none of
        // which ever start the agent task) carries `None` here. A bare
        // overwrite would erase the one thing that makes a run findable from
        // `cron logs` if its process is later killed by its own watchdog
        // before it can call this at all.
        tx.execute(
            "UPDATE runs SET state = ?2, reason = ?3, finished_at = ?4, duration_ms = ?5, \
             exit_code = ?6, timed_out = ?7, changed = ?8, \
             agent_task_id = COALESCE(?9, agent_task_id), tokens_used = ?10, \
             pending_skills = ?11, preview = ?12, stdout = ?13, stderr = ?14 WHERE id = ?1",
            params![
                run_id,
                outcome.state.as_str(),
                outcome.reason.map(|reason| reason.as_str()),
                now,
                outcome.duration_ms as i64,
                i64::from(outcome.exit_code),
                i64::from(outcome.timed_out),
                i64::from(changed),
                outcome.agent_task_id,
                outcome.tokens_used as i64,
                i64::from(outcome.pending_skills),
                preview(&outcome.stdout, &outcome.stderr),
                clamp_stream(&outcome.stdout),
                clamp_stream(&outcome.stderr),
            ],
        )?;

        let failed = i64::from(outcome.state.counts_as_failure());
        let skipped = i64::from(outcome.state == RunState::Skipped);
        tx.execute(
            "UPDATE jobs SET claimed_by = NULL, claimed_until = NULL, updated_at = ?2, \
             last_digest = COALESCE(?3, last_digest), \
             run_count = run_count + 1, fail_count = fail_count + ?4, \
             consecutive_failures = CASE WHEN ?4 = 1 THEN consecutive_failures + 1 ELSE 0 END, \
             consecutive_skips = CASE WHEN ?5 = 1 THEN consecutive_skips + 1 ELSE 0 END \
             WHERE id = ?1",
            params![
                job_id,
                now,
                outcome.digest.map(|digest| digest as i64),
                failed,
                skipped,
            ],
        )?;

        let (failures, skips): (i64, i64) = tx.query_row(
            "SELECT consecutive_failures, consecutive_skips FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if let Some(kind) = incident_for(outcome.state, outcome.reason) {
            open_incident_in(
                &tx,
                Some(job_id),
                kind,
                &incident_detail(outcome),
                outcome.agent_task_id.as_deref(),
                now,
            )?;
        } else if failures >= STREAK_TO_INCIDENT {
            open_incident_in(
                &tx,
                Some(job_id),
                IncidentKind::Failing,
                &format!("{failures} runs in a row have failed"),
                None,
                now,
            )?;
        } else if skips >= STREAK_TO_INCIDENT && outcome.reason == Some(RunReason::AgentBusy) {
            open_incident_in(
                &tx,
                Some(job_id),
                IncidentKind::LockStarvation,
                &format!("{skips} runs in a row were skipped; another agent task holds the lock"),
                None,
                now,
            )?;
        }

        // A success retires the "it keeps failing" report, but never one that
        // is waiting on a person: a granted permission is not implied by a run
        // that happened to get further.
        if outcome.state == RunState::Succeeded {
            tx.execute(
                "UPDATE incidents SET acked_at = ?2 \
                 WHERE job_id = ?1 AND acked_at IS NULL AND kind IN ('failing', 'lock-starvation')",
                params![job_id, now],
            )?;
        }

        prune_runs(&tx, job_id, now)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn reap_leases(&self, now: i64) -> Result<usize> {
        // Both statements must commit together: without a transaction, a
        // driver's `claim_due_jobs` can slip in between them and re-claim a
        // job right after its lease is cleared, so the second statement's
        // `claimed_by IS NULL` subquery no longer matches it - leaving the
        // dead owner's abandoned run stuck at `running` forever instead of
        // being closed out as a timeout.
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let released = tx.execute(
            "UPDATE jobs SET claimed_by = NULL, claimed_until = NULL \
             WHERE claimed_until IS NOT NULL AND claimed_until <= ?1",
            [now],
        )?;
        if released > 0 {
            // The jobs whose abandoned run is about to be closed out below -
            // read before the `UPDATE runs` closes it, so each one can be
            // charged for it the same way an ordinary failure is.
            let abandoned_jobs: Vec<i64> = {
                let mut statement = tx.prepare(
                    "SELECT DISTINCT job_id FROM runs WHERE state IN ('queued', 'running') \
                     AND job_id IN (SELECT id FROM jobs WHERE claimed_by IS NULL) \
                     AND finished_at IS NULL",
                )?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            // A run left behind by a dead owner is not a failure anyone saw;
            // record it so `cron history` does not simply lose it.
            tx.execute(
                "UPDATE runs SET state = 'failed', reason = 'timeout', finished_at = ?1 \
                 WHERE state IN ('queued', 'running') AND job_id IN \
                   (SELECT id FROM jobs WHERE claimed_by IS NULL) AND finished_at IS NULL",
                [now],
            )?;

            // A dead owner's abandoned run is not an ordinary failure a
            // later tick will simply retry past: unlike a command that
            // failed on its own, leaving this uncounted would mean
            // `run_count`/`fail_count` never move, `consecutive_failures`
            // never crosses `STREAK_TO_INCIDENT`, and `IncidentKind::LeaseLost`
            // - built for exactly "a claim expired while its owner was
            // presumed alive" - never actually fires. A job whose process
            // keeps dying mid-run would then silently never surface as
            // failing anywhere.
            for job_id in abandoned_jobs {
                tx.execute(
                    "UPDATE jobs SET run_count = run_count + 1, fail_count = fail_count + 1, \
                     consecutive_failures = consecutive_failures + 1 WHERE id = ?1",
                    [job_id],
                )?;
                open_incident_in(
                    &tx,
                    Some(job_id),
                    IncidentKind::LeaseLost,
                    "the process running this job disappeared before it finished; its last \
                     claim was reclaimed after the lease expired",
                    None,
                    now,
                )?;
            }
        }
        tx.commit()?;
        Ok(released)
    }

    pub(super) fn record_incident(
        &self,
        job_id: Option<i64>,
        kind: IncidentKind,
        detail: &str,
        agent_task_id: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = open_incident_in(&tx, job_id, kind, detail, agent_task_id, now)?;
        tx.commit()?;
        Ok(id)
    }

    pub(super) fn acknowledge_incident(&self, id: i64, now: i64) -> Result<CronIncident> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let acked = tx.execute(
            "UPDATE incidents SET acked_at = ?2 WHERE id = ?1 AND acked_at IS NULL",
            params![id, now],
        )?;
        if acked == 0 {
            bail!("incident {id} is unknown or already acknowledged");
        }
        let incident =
            tx.query_row(&format!("{SELECT_INCIDENT} WHERE i.id = ?1"), [id], |row| {
                incident_from_row(row)
            })?;

        // The block belongs to the set of open incidents, not to any one of
        // them: clearing the last blocking report is what lets the job run.
        if let Some(job_id) = incident.job_id {
            let still_blocked: i64 = tx.query_row(
                "SELECT COUNT(*) FROM incidents WHERE job_id = ?1 AND acked_at IS NULL \
                 AND kind IN ('approval','hook-ask','reconcile','provider','config','root-changed')",
                [job_id],
                |row| row.get(0),
            )?;
            if still_blocked == 0 {
                tx.execute(
                    "UPDATE jobs SET blocked = 0, updated_at = ?2 WHERE id = ?1",
                    params![job_id, now],
                )?;
            }
        }
        tx.commit()?;
        Ok(incident)
    }
}

/// Opens an incident unless the same job already has that kind open.
///
/// Without the dedupe, a job that needs a permission it will never be given
/// would file one report per tick and bury every other job's.
fn open_incident_in(
    tx: &rusqlite::Transaction<'_>,
    job_id: Option<i64>,
    kind: IncidentKind,
    detail: &str,
    agent_task_id: Option<&str>,
    now: i64,
) -> Result<i64> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM incidents WHERE acked_at IS NULL AND kind = ?1 \
             AND (job_id IS ?2 OR (job_id IS NULL AND ?2 IS NULL))",
            params![kind.as_str(), job_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }

    tx.execute(
        "INSERT INTO incidents(job_id, opened_at, kind, detail, agent_task_id) \
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![job_id, now, kind.as_str(), detail, agent_task_id],
    )?;
    let id = tx.last_insert_rowid();

    if kind.blocks_the_job()
        && let Some(job_id) = job_id
    {
        tx.execute(
            "UPDATE jobs SET blocked = 1, updated_at = ?2 WHERE id = ?1",
            params![job_id, now],
        )?;
    }
    Ok(id)
}

fn incident_detail(outcome: &RunOutcome) -> String {
    let reason = outcome
        .reason
        .map(|reason| reason.to_string())
        .unwrap_or_else(|| outcome.state.to_string());
    let note = preview(&outcome.stdout, &outcome.stderr);
    if note.is_empty() {
        reason
    } else {
        format!("{reason}: {note}")
    }
}

/// Picks which stream `exec::preview` reads from - stderr, preferring it
/// since that is where the reason a run failed usually is - then defers the
/// actual first-line/ANSI-strip/length-clamp work to that single primitive
/// rather than a second copy of it. `exec::preview`'s own tests
/// (`exec/tests.rs`) pin the exact truncation shape (`PREVIEW_CHARS`, the
/// trailing `…`), so this must stay a thin wrapper, not grow logic of its
/// own that could drift from them.
fn preview(stdout: &str, stderr: &str) -> String {
    let source = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    super::super::exec::preview(source)
}

/// Trims a job's history, on a fraction of runs rather than every one.
fn prune_runs(tx: &rusqlite::Transaction<'_>, job_id: i64, now: i64) -> Result<()> {
    let total: i64 = tx.query_row(
        "SELECT run_count FROM jobs WHERE id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    if !(total as u64).is_multiple_of(PRUNE_EVERY) {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM runs WHERE job_id = ?1 AND (finished_at < ?2 OR id NOT IN \
           (SELECT id FROM runs WHERE job_id = ?1 ORDER BY scheduled_for DESC LIMIT ?3))",
        params![job_id, now - RUN_HISTORY_MAX_AGE_SECS, RUN_HISTORY_LIMIT],
    )?;
    Ok(())
}
