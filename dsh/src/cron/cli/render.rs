//! Turning store views into what a person, or `--json`, sees.

use dsh_types::cron::job::{CronIncident, CronJobView, CronRun, RunState};
use std::borrow::Cow;
use tabled::{Table, Tabled};

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
        command: job.command.clone(),
    }
}

pub fn render_job_list(jobs: &[CronJobView], now: i64) -> String {
    if jobs.is_empty() {
        return "No cron jobs. Add one: cron add --name NAME '<schedule>' <command...>".to_string();
    }
    let rows: Vec<JobRow> = jobs.iter().map(|job| job_row(job, now)).collect();
    Table::new(rows).to_string()
}

struct RunRow {
    when: String,
    job: String,
    state: String,
    duration: String,
    detail: String,
}

impl Tabled for RunRow {
    const LENGTH: usize = 5;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(&self.when),
            Cow::Borrowed(&self.job),
            Cow::Borrowed(&self.state),
            Cow::Borrowed(&self.duration),
            Cow::Borrowed(&self.detail),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        ["when", "job", "state", "duration", "detail"]
            .into_iter()
            .map(Cow::Borrowed)
            .collect()
    }
}

fn run_row(run: &CronRun) -> RunRow {
    let detail = match (run.reason, run.agent_task_id.as_deref()) {
        (Some(reason), Some(task)) => format!("{reason} (task {task})"),
        (Some(reason), None) => reason.to_string(),
        (None, Some(task)) => format!("task {task}"),
        (None, None) => run.preview.clone(),
    };
    RunRow {
        when: run
            .finished_at
            .map(|at| at.to_string())
            .unwrap_or_else(|| run.started_at.map(|at| at.to_string()).unwrap_or_default()),
        job: run.job_name.clone(),
        state: run.state.to_string(),
        duration: format!("{:.1}s", run.duration_ms as f64 / 1000.0),
        detail,
    }
}

pub fn render_history(runs: &[CronRun]) -> String {
    if runs.is_empty() {
        return "No runs recorded yet.".to_string();
    }
    Table::new(runs.iter().map(run_row).collect::<Vec<_>>()).to_string()
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
