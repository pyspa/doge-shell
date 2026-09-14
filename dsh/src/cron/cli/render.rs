//! Turning store views into what a person, or `--json`, sees.

use crate::cron::clock;
use dsh_types::cron::job::{CronIncident, CronJobView, CronRun, RunOutput, RunState};
use dsh_types::text::clamp_chars;
use std::borrow::Cow;
use tabled::{Table, Tabled};

/// First 8 characters of an id (a run id or an agent task id, both UUIDs) -
/// enough to be unambiguous in practice and short enough for a table column.
/// `cron logs --run` accepts any unique prefix, this or longer.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// `at`, as a local `YYYY-MM-DD HH:MM` a person can read at a glance, instead
/// of a raw Unix timestamp. Falls back to the timestamp itself on the (never
/// observed in practice) chance the conversion fails, rather than hiding the
/// row.
fn format_when(at: i64) -> String {
    match clock::civil_from_epoch(at) {
        Some(civil) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            civil.year, civil.month, civil.day, civil.hour, civil.minute
        ),
        None => at.to_string(),
    }
}

pub fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// `next_run_at` as "in 5m" relative to `now`, or a fixed label for a job the
/// wall clock will never fire.
pub fn describe_next(job: &CronJobView, now: i64) -> String {
    if job.paused {
        return "paused".to_string();
    }
    if job.blocked {
        return "blocked".to_string();
    }
    match job.next_run_at {
        Some(at) if at <= now => "due".to_string(),
        Some(at) => format!("in {}", format_duration((at - now) as u64)),
        None => "-".to_string(),
    }
}

pub fn describe_last(run: Option<&CronRun>) -> String {
    let Some(run) = run else {
        return "-".to_string();
    };
    let status = match run.state {
        RunState::Succeeded => "ok".to_string(),
        _ if run.timed_out => "timeout".to_string(),
        RunState::Skipped => format!(
            "skipped ({})",
            run.reason.map(|r| r.to_string()).unwrap_or_default()
        ),
        RunState::NeedsApproval => "needs-approval".to_string(),
        _ => format!("exit {}", run.exit_code),
    };
    format!("{status} {:.1}s", run.duration_ms as f64 / 1000.0)
}

struct JobRow {
    id: String,
    name: String,
    kind: String,
    schedule: String,
    state: String,
    next: String,
    last: String,
    runs: String,
    command: String,
}

impl Tabled for JobRow {
    const LENGTH: usize = 9;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(&self.id),
            Cow::Borrowed(&self.name),
            Cow::Borrowed(&self.kind),
            Cow::Borrowed(&self.schedule),
            Cow::Borrowed(&self.state),
            Cow::Borrowed(&self.next),
            Cow::Borrowed(&self.last),
            Cow::Borrowed(&self.runs),
            Cow::Borrowed(&self.command),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        [
            "id", "name", "kind", "schedule", "state", "next", "last", "runs", "command",
        ]
        .into_iter()
        .map(Cow::Borrowed)
        .collect()
    }
}

/// Longest a `command` cell may run before the table itself becomes the
/// wrong tool - same length as `DETAIL_PREVIEW_CHARS` below, so a cell in
/// either table follows one rule. An agent job's `command` is its goal in
/// full, which otherwise blows the table out to hundreds of columns; the
/// full text stays reachable through `cron show <job>` and `--json`, neither
/// of which clamps it.
const JOB_LIST_COMMAND_CHARS: usize = 60;

fn job_row(job: &CronJobView, now: i64) -> JobRow {
    JobRow {
        id: job.id.to_string(),
        name: job.name.clone(),
        kind: job.kind.to_string(),
        schedule: job.schedule_spec.clone(),
        state: job.state_label().to_string(),
        next: describe_next(job, now),
        last: describe_last(job.last.as_ref()),
        runs: if job.fail_count > 0 {
            format!("{} ({} failed)", job.run_count, job.fail_count)
        } else {
            job.run_count.to_string()
        },
        command: clamp_chars(&job.command, JOB_LIST_COMMAND_CHARS),
    }
}

pub fn render_job_list(jobs: &[CronJobView], now: i64) -> String {
    if jobs.is_empty() {
        return "No cron jobs. Add one: cron add --name NAME '<schedule>' <command...>".to_string();
    }
    let rows: Vec<JobRow> = jobs.iter().map(|job| job_row(job, now)).collect();
    Table::new(rows).to_string()
}

/// Longest a `detail` cell's `preview` part may run before `cron logs
/// --run <id>` is the better tool - the full 120-character `preview` would
/// make the table itself hard to read.
const DETAIL_PREVIEW_CHARS: usize = 60;

struct RunRow {
    when: String,
    run: String,
    job: String,
    state: String,
    duration: String,
    detail: String,
}

impl Tabled for RunRow {
    const LENGTH: usize = 6;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(&self.when),
            Cow::Borrowed(&self.run),
            Cow::Borrowed(&self.job),
            Cow::Borrowed(&self.state),
            Cow::Borrowed(&self.duration),
            Cow::Borrowed(&self.detail),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        ["when", "run", "job", "state", "duration", "detail"]
            .into_iter()
            .map(Cow::Borrowed)
            .collect()
    }
}

/// Builds the `detail` cell from whichever of reason/task-id/preview this run
/// actually has, instead of the four-way exclusive match this replaced -
/// which meant a failed AI run (always has both a reason and a task id) never
/// showed its `preview` at all, no matter how informative it was.
fn run_detail(run: &CronRun) -> String {
    let mut parts = Vec::new();
    if let Some(reason) = run.reason {
        parts.push(reason.to_string());
    }
    if let Some(task) = &run.agent_task_id {
        parts.push(format!("task {}", short_id(task)));
    }
    if !run.preview.is_empty() {
        parts.push(clamp_chars(&run.preview, DETAIL_PREVIEW_CHARS));
    }
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(" - ")
    }
}

fn run_row(run: &CronRun) -> RunRow {
    let when = run
        .finished_at
        .or(run.started_at)
        .map(format_when)
        .unwrap_or_default();
    RunRow {
        when,
        run: short_id(&run.id).to_string(),
        job: run.job_name.clone(),
        state: run.state.to_string(),
        duration: format!("{:.1}s", run.duration_ms as f64 / 1000.0),
        detail: run_detail(run),
    }
}

pub fn render_history(runs: &[CronRun]) -> String {
    if runs.is_empty() {
        return "No runs recorded yet.".to_string();
    }
    Table::new(runs.iter().map(run_row).collect::<Vec<_>>()).to_string()
}

/// `cron logs`' own view: one run's stored `stdout`/`stderr`, in full.
///
/// With both streams selected (the default) each is labelled, so the split
/// is unambiguous; a single stream is printed bare, on purpose, so `cron logs
/// job --stdout | …` pipes exactly what the job printed and nothing else.
/// Carries no trailing newline of its own - `Context::write_stdout` always
/// adds exactly one (`writeln!`), so a section ending in `\n` here would
/// double it, exactly the bug this was written to fix.
pub fn render_run_output(output: &RunOutput, show_stdout: bool, show_stderr: bool) -> String {
    let mut sections = Vec::new();
    let labelled = show_stdout && show_stderr;
    if show_stdout {
        sections.push(stream_section("stdout", &output.stdout, labelled));
    }
    if show_stderr {
        sections.push(stream_section("stderr", &output.stderr, labelled));
    }
    sections.join("\n")
}

/// `pub(in crate::cron)`, not private: `handlers/logs.rs`'s live-reconstructed
/// path renders its `stderr` section through this too, so a run whose
/// `stdout` had to be reconstructed still gets the identical `(empty)`/label
/// treatment as the ordinary path instead of a hand-rolled variant that
/// quietly drifts from it (e.g. omitting the section outright when `stderr`
/// is empty, unlike this function).
///
/// Never carries a trailing newline: `text` is a job's recorded stream and
/// almost always ends in one already (its own last `println`/`echo`), so
/// appending another here - on top of `write_stdout`'s own `writeln!` - is
/// exactly how `cron logs job --stdout` used to end up with two or three
/// blank lines for one line of actual output.
pub(in crate::cron) fn stream_section(name: &str, text: &str, labelled: bool) -> String {
    let body = if text.is_empty() {
        "(empty)"
    } else {
        text.trim_end_matches('\n')
    };
    if labelled {
        format!("--- {name} ---\n{body}")
    } else {
        body.to_string()
    }
}

struct IncidentRow {
    id: String,
    job: String,
    kind: String,
    detail: String,
    state: String,
}

impl Tabled for IncidentRow {
    const LENGTH: usize = 5;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(&self.id),
            Cow::Borrowed(&self.job),
            Cow::Borrowed(&self.kind),
            Cow::Borrowed(&self.detail),
            Cow::Borrowed(&self.state),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        ["id", "job", "kind", "detail", "state"]
            .into_iter()
            .map(Cow::Borrowed)
            .collect()
    }
}

fn incident_row(incident: &CronIncident) -> IncidentRow {
    IncidentRow {
        id: incident.id.to_string(),
        job: incident.job_name.clone().unwrap_or_else(|| "-".to_string()),
        kind: incident.kind.to_string(),
        detail: incident.detail.clone(),
        state: if incident.is_open() { "open" } else { "acked" }.to_string(),
    }
}

pub fn render_incidents(incidents: &[CronIncident]) -> String {
    if incidents.is_empty() {
        return "No incidents.".to_string();
    }
    Table::new(incidents.iter().map(incident_row).collect::<Vec<_>>()).to_string()
}

#[cfg(test)]
mod tests;
