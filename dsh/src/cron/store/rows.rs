//! Turning rows into types, and selectors into ids.
//!
//! Split out of `store.rs` only for size, but the column-index coupling is
//! real: each `SELECT_*` constant and the mapper beneath it are one unit, and
//! reordering a column without moving its `row.get(n)` compiles cleanly and
//! returns the wrong field. Keep each pair adjacent.

use anyhow::{Result, bail};
use dsh_types::cron::job::{
    AgentJobSpec, CronIncident, CronJobView, CronRun, IncidentKind, JobKind, RunReason, RunState,
    RunTrigger,
};
use dsh_types::schedule::{NotifyPolicy, parse_schedule};
use rusqlite::{Connection, OptionalExtension, Row};

/// Kept per run, masked. Enough to see what happened without turning the
/// database into a log shipper.
pub(super) const MAX_STREAM_BYTES: usize = 8 * 1024;

/// Keeps the head and the tail of a stream, which is where the useful parts of
/// a failure are. Byte-based, and stepped back to a character boundary so a
/// multi-byte character is never cut in half.
pub(super) fn clamp_stream(text: &str) -> String {
    if text.len() <= MAX_STREAM_BYTES {
        return text.to_string();
    }
    let half = MAX_STREAM_BYTES / 2;
    let head = &text[..text.floor_char_boundary(half)];
    let tail = &text[text.ceil_char_boundary(text.len() - half)..];
    format!("{head}\n... [truncated] ...\n{tail}")
}

pub(super) const SELECT_JOB: &str = "SELECT id, name, kind, schedule_spec, command, payload, cwd, env, \
     notify, timeout_secs, catchup_secs, enabled, blocked, next_run_at, last_digest, \
     run_count, fail_count, consecutive_failures, claimed_by FROM jobs";

pub(super) fn job_from_row(row: &Row<'_>) -> rusqlite::Result<CronJobView> {
    let schedule_spec: String = row.get(3)?;
    let payload: Option<String> = row.get(5)?;
    let notify: String = row.get(8)?;
    let kind: String = row.get(2)?;
    let claimed_by: Option<String> = row.get(18)?;
    Ok(CronJobView {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: JobKind::parse(&kind).map_err(to_sql_error)?,
        schedule: parse_schedule(&schedule_spec).map_err(to_sql_error)?,
        schedule_spec,
        command: row.get(4)?,
        agent: decode_payload(payload.as_deref()).map_err(to_sql_error)?,
        cwd: row.get(6)?,
        notify: NotifyPolicy::parse(&notify).map_err(to_sql_error)?,
        timeout_secs: row.get::<_, i64>(9)? as u64,
        catchup_secs: row.get::<_, i64>(10)? as u64,
        paused: row.get::<_, i64>(11)? == 0,
        blocked: row.get::<_, i64>(12)? != 0,
        next_run_at: row.get(13)?,
        running: claimed_by.is_some(),
        run_count: row.get::<_, i64>(15)? as u64,
        fail_count: row.get::<_, i64>(16)? as u64,
        consecutive_failures: row.get::<_, i64>(17)? as u32,
        last: None,
    })
}

pub(super) fn to_sql_error(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

pub(super) fn decode_payload(payload: Option<&str>) -> Result<Option<AgentJobSpec>, String> {
    match payload {
        None => Ok(None),
        Some(text) => serde_json::from_str(text)
            .map(Some)
            .map_err(|error| format!("job payload is unreadable: {error}")),
    }
}

pub(super) const SELECT_RUN: &str = "SELECT r.id, r.job_id, j.name, r.scheduled_for, r.state, r.reason, \
     r.started_at, r.finished_at, r.duration_ms, r.exit_code, r.timed_out, r.changed, \
     r.trigger, r.agent_task_id, r.tokens_used, r.pending_skills, r.preview \
     FROM runs r JOIN jobs j ON j.id = r.job_id";

pub(super) fn run_from_row(row: &Row<'_>) -> rusqlite::Result<CronRun> {
    let state: String = row.get(4)?;
    let reason: Option<String> = row.get(5)?;
    let trigger: String = row.get(12)?;
    Ok(CronRun {
        id: row.get(0)?,
        job_id: row.get(1)?,
        job_name: row.get(2)?,
        scheduled_for: row.get(3)?,
        state: RunState::parse(&state).map_err(to_sql_error)?,
        reason: reason
            .map(|text| RunReason::parse(&text))
            .transpose()
            .map_err(to_sql_error)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
        duration_ms: row.get::<_, i64>(8)? as u64,
        exit_code: row.get::<_, i64>(9)? as i32,
        timed_out: row.get::<_, i64>(10)? != 0,
        changed: row.get::<_, i64>(11)? != 0,
        trigger: RunTrigger::parse(&trigger).map_err(to_sql_error)?,
        agent_task_id: row.get(13)?,
        tokens_used: row.get::<_, i64>(14)? as u64,
        pending_skills: row.get::<_, i64>(15)? as u32,
        preview: row.get(16)?,
    })
}

pub(super) const SELECT_INCIDENT: &str = "SELECT i.id, i.job_id, j.name, i.opened_at, i.acked_at, i.kind, \
     i.detail, i.agent_task_id FROM incidents i LEFT JOIN jobs j ON j.id = i.job_id";

pub(super) fn incident_from_row(row: &Row<'_>) -> rusqlite::Result<CronIncident> {
    let kind: String = row.get(5)?;
    Ok(CronIncident {
        id: row.get(0)?,
        job_id: row.get(1)?,
        job_name: row.get(2)?,
        opened_at: row.get(3)?,
        acked_at: row.get(4)?,
        kind: IncidentKind::parse(&kind).map_err(to_sql_error)?,
        detail: row.get(6)?,
        agent_task_id: row.get(7)?,
    })
}

/// A job id, from either spelling a person might use.
///
/// Names win over ids, so a job literally named `3` still resolves to itself.
pub(super) fn resolve(connection: &Connection, selector: &str) -> Result<i64> {
    if let Some(id) = connection
        .query_row("SELECT id FROM jobs WHERE name = ?1", [selector], |row| {
            row.get::<_, i64>(0)
        })
        .optional()?
    {
        return Ok(id);
    }
    if let Ok(id) = selector.parse::<i64>()
        && let Some(found) = connection
            .query_row("SELECT id FROM jobs WHERE id = ?1", [id], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
    {
        return Ok(found);
    }
    bail!("{selector}: no such cron job")
}

pub(super) fn job_name(connection: &Connection, id: i64) -> Result<String> {
    Ok(
        connection.query_row("SELECT name FROM jobs WHERE id = ?1", [id], |row| {
            row.get(0)
        })?,
    )
}
