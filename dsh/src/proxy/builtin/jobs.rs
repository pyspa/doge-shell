//! Job control command handlers (jobs, fg, bg).

mod bg;
mod fg;
mod list;
#[cfg(test)]
mod tests;
mod wait;
pub use bg::execute_bg;
pub use fg::execute_fg;
#[allow(unused_imports)]
pub(crate) use fg::{
    block_on_job_control_future, finalize_background_resume, finalize_foreground_job,
    foreground_selected_job, run_foreground_driver,
};
pub use list::execute_jobs;

/// A `%`-prefixed job specification, syntax only (no table lookup).
///
/// `wait` resolves bare decimals as PIDs, never as job numbers, so this
/// type only ever represents the `%`-prefixed forms. `fg`/`bg` keep their
/// legacy bare-number behavior through [`parse_job_spec`], which funnels
/// into this type internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobSpec {
    /// `%`, `%+` or `%%`: the most recently added active job.
    Current,
    /// `%-`: the active job before the current one.
    Previous,
    /// `%N`: the active job with this stable job number.
    Number(usize),
}

/// Parse a `%`-prefixed job specification without touching any job table.
///
/// Accepts `%`/`%%`/`%+` (current), `%-` (previous), `%N` (job number).
/// Rejects everything else — including bare numbers, which `wait` must
/// read as PIDs.
pub(crate) fn parse_percent_job_spec(spec: &str) -> Option<JobSpec> {
    let spec = spec.trim();
    match spec {
        "%" | "%%" | "%+" => Some(JobSpec::Current),
        "%-" => Some(JobSpec::Previous),
        _ => {
            let digits = spec.strip_prefix('%')?;
            // `%foo`, `%?foo`, empty digits: not a job spec here.
            let number: usize = digits.parse().ok()?;
            Some(JobSpec::Number(number))
        }
    }
}

/// Resolve a [`JobSpec`] against the active job table only.
///
/// `Current`/`Previous` are active-table concepts: they never fall back to
/// the completed ledger (a reaped job is not "current"). `Number(n)` also
/// resolves here when the job is still active; the completed-ledger
/// fallback for explicit `%N` lives in the `wait` layer, which owns both
/// the table and the ledger.
pub(crate) fn resolve_active_job_spec(
    spec: JobSpec,
    wait_jobs: &[crate::process::Job],
) -> Option<usize> {
    let selection = crate::shell::job_selection::ActiveJobSelection::for_len(wait_jobs.len());
    match spec {
        JobSpec::Current => selection.current(),
        JobSpec::Previous => selection.previous(),
        JobSpec::Number(number) => wait_jobs.iter().position(|job| job.job_id == number),
    }
}

/// Parse a legacy `fg`/`bg` job specification (e.g., "%1", "1", "%", "%+", "%-").
///
/// Returns the job index in wait_jobs vector, or None if not found.
pub fn parse_job_spec(spec: &str, wait_jobs: &[crate::process::Job]) -> Option<usize> {
    // Legacy `fg`/`bg` behavior: empty means current, bare `+`/`-`/`N`
    // alias their `%`-prefixed forms. `wait` never calls this: its bare
    // decimals are PIDs.
    if spec.is_empty() {
        return resolve_active_job_spec(JobSpec::Current, wait_jobs);
    }
    let trimmed = spec.trim();
    if trimmed == "+" {
        return resolve_active_job_spec(JobSpec::Current, wait_jobs);
    }
    if trimmed == "-" {
        return resolve_active_job_spec(JobSpec::Previous, wait_jobs);
    }
    if let Some(parsed) = parse_percent_job_spec(trimmed) {
        return resolve_active_job_spec(parsed, wait_jobs);
    }
    // Bare job number (legacy `fg 1` / `bg 1` only).
    if let Ok(number) = trimmed.parse::<usize>() {
        return resolve_active_job_spec(JobSpec::Number(number), wait_jobs);
    }
    None
}

impl dsh_builtin::shell_capabilities::JobControlCapability for crate::shell::Shell {
    fn wait_for_jobs(
        &mut self,
        ctx: &dsh_types::Context,
        argv: Vec<String>,
    ) -> anyhow::Result<i32> {
        wait::execute_wait(self, ctx, argv)
    }

    fn foreground_job(
        &mut self,
        ctx: &dsh_types::Context,
        argv: Vec<String>,
    ) -> anyhow::Result<i32> {
        fg::execute_fg(self, ctx, argv)
    }
}
