//! The `jobs` builtin: list background jobs.
//!
//! The table is reconciled before listing (see [`execute_jobs`]) so a
//! completed re-exec helper or external child never lists as `running`.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use std::borrow::Cow;
use tabled::{Table, Tabled};

struct Job {
    job: usize,
    pid: i32,
    state: String,
    command: String,
}

impl Tabled for Job {
    const LENGTH: usize = 4;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Owned(self.job.to_string()),
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

/// Execute the `jobs` builtin command.
///
/// Lists all background jobs. This builtin has a side effect: the table is
/// reconciled before listing.
///
/// Each job's canonical process tree is polled (non-blocking, no new
/// waiters) and jobs whose tree has completed leave the table here, exactly
/// as `check_job_state` would remove them — but silently, with no completion
/// notice and without draining output monitors (`check_job_state` drains
/// background output first; in practice the foreground/background waits
/// already drained it). Without this, non-interactive mode — which has no
/// background tick — would list long-dead re-exec helpers as `running`
/// forever.
pub fn execute_jobs(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    let mut index = 0;
    while index < shell.wait_jobs.len() {
        if shell.wait_jobs[index].update_status() {
            shell.wait_jobs.remove(index);
        } else {
            index += 1;
        }
    }
    if shell.wait_jobs.is_empty() {
        ctx.write_stdout("jobs: there are no jobs")?;
    } else {
        let jobs: Vec<Job> = shell
            .wait_jobs
            .iter()
            .map(|job| Job {
                job: job.job_id,
                pid: job.pid.map(|p| p.as_raw()).unwrap_or(-1),
                state: format!("{}", job.state),
                command: job.cmd.clone(),
            })
            .collect();
        let table = Table::new(jobs).to_string();
        ctx.write_stdout(table.as_str())?;
    }
    Ok(())
}
