//! The `jobs` builtin: list background jobs.
//!
//! The table is reconciled before listing (see [`execute_jobs`]) so a
//! completed re-exec helper or external child never lists as `running`.
//!
//! CLI contract: `jobs [options] [jobspec]` where options are `-l`/`--list`
//! (long table with associated pids) and `-p`/`--pgid` (machine-readable
//! process-group leader IDs only). At most one jobspec filters the table.
//! Argument syntax is validated before reconciliation so an invalid option
//! never triggers a table side effect.

use crate::shell::Shell;
use crate::shell::job_selection::{ActiveJobMarker, ActiveJobSelection};
use anyhow::Result;
use dsh_types::Context;
use std::borrow::Cow;
use tabled::{Table, Tabled};

/// Output shape selected by `jobs` options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobsOutputMode {
    /// `job` / `state` / `command` table.
    Default,
    /// `job` / `pid` / `state` / `command` table.
    Long,
    /// Raw numeric process-group IDs, one per line, no header.
    PgidOnly,
}

/// A validated `jobs` invocation: output mode plus optional jobspec filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobsInvocation {
    pub(crate) mode: JobsOutputMode,
    pub(crate) jobspec: Option<String>,
}

/// Parse `jobs` argv (including `argv[0]`) without touching the job table.
///
/// Accepts `-l`/`--list`, `-p`/`--pgid`, and a `--` separator. Repeated
/// same-mode bundles (`-ll`, `-pp`) are allowed; mixing output modes
/// (`-lp`, `-l -p`) is a usage error, as is any unknown option. `-` and `+`
/// are previous/current job aliases, never options. At most one jobspec
/// operand is accepted.
///
/// Errors are prefixless; the builtin wrapper owns the final `jobs: ` prefix.
pub(crate) fn parse_jobs_invocation(argv: &[String]) -> Result<JobsInvocation> {
    let mut mode = JobsOutputMode::Default;
    let mut mode_seen = false;
    let mut jobspec: Option<String> = None;
    let mut end_of_options = false;

    for arg in argv.iter().skip(1) {
        if end_of_options {
            if jobspec.is_some() {
                return Err(anyhow::anyhow!("too many arguments"));
            }
            jobspec = Some(arg.clone());
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            continue;
        }
        // `-` / `+` are job aliases, never options.
        if arg == "-" || arg == "+" {
            if jobspec.is_some() {
                return Err(anyhow::anyhow!("too many arguments"));
            }
            jobspec = Some(arg.clone());
            continue;
        }
        if arg == "--list" || arg == "-l" {
            set_jobs_mode(&mut mode, &mut mode_seen, JobsOutputMode::Long)?;
            continue;
        }
        if arg == "--pgid" || arg == "-p" {
            set_jobs_mode(&mut mode, &mut mode_seen, JobsOutputMode::PgidOnly)?;
            continue;
        }
        if arg.strip_prefix("--").is_some() {
            return Err(anyhow::anyhow!("unsupported option: {arg}"));
        }
        if let Some(short) = arg.strip_prefix('-') {
            // Short bundles of one repeated mode flag (`-ll`, `-pp`).
            let chars: Vec<char> = short.chars().collect();
            if !chars.is_empty() && chars.iter().all(|c| *c == 'l') {
                set_jobs_mode(&mut mode, &mut mode_seen, JobsOutputMode::Long)?;
                continue;
            }
            if !chars.is_empty() && chars.iter().all(|c| *c == 'p') {
                set_jobs_mode(&mut mode, &mut mode_seen, JobsOutputMode::PgidOnly)?;
                continue;
            }
            return Err(anyhow::anyhow!("unsupported option: {arg}"));
        }
        if jobspec.is_some() {
            return Err(anyhow::anyhow!("too many arguments"));
        }
        jobspec = Some(arg.clone());
    }

    Ok(JobsInvocation { mode, jobspec })
}

fn set_jobs_mode(mode: &mut JobsOutputMode, seen: &mut bool, next: JobsOutputMode) -> Result<()> {
    if *seen && *mode != next {
        return Err(anyhow::anyhow!("options -l and -p are mutually exclusive"));
    }
    *mode = next;
    *seen = true;
    Ok(())
}

struct DefaultJobRow {
    job: String,
    state: String,
    command: String,
}

impl Tabled for DefaultJobRow {
    const LENGTH: usize = 3;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(self.job.as_str()),
            Cow::Borrowed(self.state.as_str()),
            Cow::Borrowed(self.command.as_str()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![
            Cow::Borrowed("job"),
            Cow::Borrowed("state"),
            Cow::Borrowed("command"),
        ]
    }
}

struct LongJobRow {
    job: String,
    pid: i32,
    state: String,
    command: String,
}

impl Tabled for LongJobRow {
    const LENGTH: usize = 4;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(self.job.as_str()),
            Cow::Owned(self.pid.to_string()),
            Cow::Borrowed(self.state.as_str()),
            Cow::Borrowed(self.command.as_str()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![
            Cow::Borrowed("job"),
            Cow::Borrowed("pid"),
            Cow::Borrowed("state"),
            Cow::Borrowed("command"),
        ]
    }
}

/// One listed job plus its marker role from the full active table.
///
/// The marker is computed from the full-table index, never from the
/// filtered slice position: `jobs %1` must not mark job 1 current just
/// because the selection holds a single entry.
pub(crate) struct JobListEntry<'a> {
    pub(crate) job: &'a crate::process::Job,
    pub(crate) marker: ActiveJobMarker,
}

fn format_job_label(job_id: usize, marker: ActiveJobMarker) -> String {
    format!("[{job_id}]{}", marker.glyph())
}

/// Render the default `jobs` table (`job` / `state` / `command`, no pid).
pub(crate) fn render_jobs_default(entries: &[JobListEntry<'_>]) -> String {
    let rows: Vec<DefaultJobRow> = entries
        .iter()
        .map(|entry| DefaultJobRow {
            job: format_job_label(entry.job.job_id, entry.marker),
            state: format!("{}", entry.job.state),
            command: entry.job.cmd.clone(),
        })
        .collect();
    Table::new(rows).to_string()
}

/// Render the long `jobs -l` table (`job` / `pid` / `state` / `command`).
pub(crate) fn render_jobs_long(entries: &[JobListEntry<'_>]) -> String {
    let rows: Vec<LongJobRow> = entries
        .iter()
        .map(|entry| LongJobRow {
            job: format_job_label(entry.job.job_id, entry.marker),
            pid: entry.job.pid.map(|p| p.as_raw()).unwrap_or(-1),
            state: format!("{}", entry.job.state),
            command: entry.job.cmd.clone(),
        })
        .collect();
    Table::new(rows).to_string()
}

/// Render `jobs -p`: raw process-group leader IDs, one per line, no header.
///
/// Fails closed when a selected active job has no process group: the caller
/// keeps table ownership and reports the error instead of emitting a
/// placeholder ID.
pub(crate) fn render_jobs_pgids(jobs: &[&crate::process::Job]) -> Result<String> {
    let mut out = String::new();
    for job in jobs {
        let Some(pgid) = job.pgid else {
            return Err(anyhow::anyhow!("job {} has no process group", job.job_id));
        };
        out.push_str(&format!("{}\n", pgid.as_raw()));
    }
    Ok(out)
}

/// Execute the `jobs` builtin command.
///
/// Argument syntax is validated first, before any table side effect. After
/// validation the table is reconciled through the same canonical
/// [`Shell::check_job_state`](crate::shell::Shell::check_job_state) path as
/// the background tick, so completed jobs drain their output monitors to
/// EOF and archive their status in the known-async ledger before leaving
/// the table. Listing never consumes retained statuses: a later
/// `wait PID` still reports them. Without reconciliation, non-interactive
/// mode — which has no background tick — would list long-dead re-exec
/// helpers as `running` forever.
///
/// Errors are prefixless; the builtin wrapper owns the final `jobs: ` prefix
/// and core failure paths never write to stderr.
pub fn execute_jobs(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    let invocation = parse_jobs_invocation(&argv)?;
    super::block_on_job_control_future(shell.check_job_state())??;
    let selection = ActiveJobSelection::for_len(shell.wait_jobs.len());
    let selected_indices: Vec<usize> = match &invocation.jobspec {
        Some(spec) => {
            let Some(index) = super::parse_job_spec(spec, &shell.wait_jobs) else {
                return Err(anyhow::anyhow!("job not found: {spec}"));
            };
            vec![index]
        }
        None => (0..shell.wait_jobs.len()).collect(),
    };
    let entries: Vec<JobListEntry<'_>> = selected_indices
        .iter()
        .map(|&index| JobListEntry {
            job: &shell.wait_jobs[index],
            marker: selection.marker_for(index),
        })
        .collect();
    // `jobs -p` stays marker-free machine-readable output.
    let selected_jobs: Vec<&crate::process::Job> = entries.iter().map(|entry| entry.job).collect();
    match invocation.mode {
        JobsOutputMode::PgidOnly => {
            let output = render_jobs_pgids(&selected_jobs)?;
            if !output.is_empty() {
                ctx.write_stdout(output.trim_end())?;
            }
        }
        JobsOutputMode::Default => {
            if entries.is_empty() {
                ctx.write_stdout("jobs: there are no jobs")?;
            } else {
                ctx.write_stdout(render_jobs_default(&entries).as_str())?;
            }
        }
        JobsOutputMode::Long => {
            if entries.is_empty() {
                ctx.write_stdout("jobs: there are no jobs")?;
            } else {
                ctx.write_stdout(render_jobs_long(&entries).as_str())?;
            }
        }
    }
    Ok(())
}
