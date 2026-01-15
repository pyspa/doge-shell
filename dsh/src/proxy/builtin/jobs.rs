//! Job control command handlers (jobs, fg, bg).

use crate::process::ProcessState;
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use nix::sys::signal::{Signal, killpg};
use tabled::{Table, Tabled};
use tracing::{debug, error, warn};

#[derive(Tabled)]
struct Job {
    job: usize,
    pid: i32,
    state: String,
    command: String,
}

/// Parse job specification (e.g., "%1", "1", "%+", "%-").
///
/// Returns the job index in wait_jobs vector, or None if not found.
pub fn parse_job_spec(spec: &str, wait_jobs: &[crate::process::Job]) -> Option<usize> {
    if spec.is_empty() {
        // Default to most recent job
        return if wait_jobs.is_empty() {
            None
        } else {
            Some(wait_jobs.len() - 1)
        };
    }

    let spec = spec.trim();

    // Handle %+ (current job) and %- (previous job)
    if spec == "%+" || spec == "+" {
        return if wait_jobs.is_empty() {
            None
        } else {
            Some(wait_jobs.len() - 1)
        };
    }
    if spec == "%-" || spec == "-" {
        return if wait_jobs.len() < 2 {
            None
        } else {
            Some(wait_jobs.len() - 2)
        };
    }

    // Handle %n or n format (job number)
    let job_num_str = if let Some(stripped) = spec.strip_prefix('%') {
        stripped
    } else {
        spec
    };

    if let Ok(job_num) = job_num_str.parse::<usize>() {
        // Find job by job_id
        for (index, job) in wait_jobs.iter().enumerate() {
            if job.job_id == job_num {
                return Some(index);
            }
        }
    }

    None
}

/// Execute the `jobs` builtin command.
///
/// Lists all background jobs.
pub fn execute_jobs(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
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

/// Execute the `fg` builtin command.
///
/// Brings a background job to the foreground.
pub fn execute_fg(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    debug!(
        "FG_CMD_START: Starting fg command - wait_jobs.len(): {}, args: {:?}",
        shell.wait_jobs.len(),
        argv
    );

    if shell.wait_jobs.is_empty() {
        debug!("FG_CMD_NO_JOBS: No jobs available for fg command");
        ctx.write_stdout("fg: there are no suitable jobs")?;
    } else {
        let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
        debug!("FG_CMD_SPEC: Job specification: '{}'", job_spec);

        // Log current job list for debugging
        debug!("FG_CMD_AVAILABLE_JOBS: Current job list:");
        for (i, job) in shell.wait_jobs.iter().enumerate() {
            debug!(
                "FG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
                i, job.job_id, job.pid, job.state, job.foreground, job.cmd
            );
        }

        if let Some(job_index) = parse_job_spec(job_spec, &shell.wait_jobs) {
            let mut job = shell.wait_jobs.remove(job_index);
            debug!(
                "FG_CMD_SELECTED: Selected job {} at index {} for foreground",
                job.job_id, job_index
            );
            debug!(
                "FG_CMD_JOB_DETAILS: Job details before fg - state: {:?}, pgid: {:?}, pid: {:?}",
                job.state, job.pgid, job.pid
            );

            ctx.write_stdout(&format!(
                "dsh: job {} '{}' to foreground",
                job.job_id, job.cmd
            ))
            .ok();

            let cont = if let ProcessState::Stopped(_, _) = job.state {
                debug!(
                    "FG_CMD_STOPPED: Job {} is stopped, will send SIGCONT",
                    job.job_id
                );
                true
            } else {
                debug!(
                    "FG_CMD_NOT_STOPPED: Job {} is not stopped, no SIGCONT needed (state: {:?})",
                    job.job_id, job.state
                );
                false
            };

            let old_state = job.state;
            job.state = ProcessState::Running;
            debug!(
                "FG_CMD_STATE_CHANGE: Set job {} state from {:?} to Running",
                job.job_id, old_state
            );

            debug!(
                "FG_CMD_FOREGROUND_CALL: About to call put_in_foreground_sync for job {} with no_hang=true, cont={}",
                job.job_id, cont
            );

            match job.put_in_foreground_sync(true, cont) {
                Ok(_) => {
                    debug!(
                        "FG_CMD_SUCCESS: put_in_foreground_sync completed successfully for job {}",
                        job.job_id
                    );
                }
                Err(err) => {
                    error!(
                        "FG_CMD_ERROR: put_in_foreground_sync failed for job {} with error: {:?}",
                        job.job_id, err
                    );
                    ctx.write_stderr(&format!("{err}")).ok();
                    return Err(err);
                }
            }
        } else {
            let error_msg = if job_spec.is_empty() {
                "fg: no current job".to_string()
            } else {
                format!("fg: job not found: {job_spec}")
            };
            debug!("FG_CMD_NOT_FOUND: {}", error_msg);
            ctx.write_stderr(&error_msg)?;
            return Err(anyhow::anyhow!(error_msg));
        }
    }
    Ok(())
}

/// Execute the `bg` builtin command.
///
/// Resumes a stopped job in the background.
pub fn execute_bg(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    debug!(
        "BG_CMD_START: Starting bg command - wait_jobs.len(): {}, args: {:?}",
        shell.wait_jobs.len(),
        argv
    );

    if shell.wait_jobs.is_empty() {
        debug!("BG_CMD_NO_JOBS: No jobs available for bg command");
        ctx.write_stdout("bg: there are no suitable jobs")?;
    } else {
        let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
        debug!("BG_CMD_SPEC: Job specification: '{}'", job_spec);

        // Log current job list for debugging
        debug!("BG_CMD_AVAILABLE_JOBS: Current job list:");
        for (i, job) in shell.wait_jobs.iter().enumerate() {
            debug!(
                "BG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
                i, job.job_id, job.pid, job.state, job.foreground, job.cmd
            );
        }

        // Find job by specification or default to most recent stopped job
        let job_index = if job_spec.is_empty() {
            debug!("BG_CMD_FIND_STOPPED: Looking for most recent stopped job");
            // Find the most recent stopped job
            let mut found_index = None;
            for (i, job) in shell.wait_jobs.iter().enumerate().rev() {
                debug!(
                    "BG_CMD_CHECK_STOPPED: Checking job {} (index: {}, state: {:?})",
                    job.job_id, i, job.state
                );
                if matches!(job.state, ProcessState::Stopped(_, _)) {
                    debug!(
                        "BG_CMD_FOUND_STOPPED: Found stopped job {} at index {}",
                        job.job_id, i
                    );
                    found_index = Some(i);
                    break;
                }
            }
            if found_index.is_none() {
                debug!("BG_CMD_NO_STOPPED: No stopped jobs found");
            }
            found_index
        } else {
            debug!(
                "BG_CMD_PARSE_SPEC: Parsing job specification: '{}'",
                job_spec
            );
            // Parse job specification
            parse_job_spec(job_spec, &shell.wait_jobs)
        };

        if let Some(index) = job_index {
            let job = &shell.wait_jobs[index];
            debug!(
                "BG_CMD_SELECTED: Selected job {} at index {} for background",
                job.job_id, index
            );

            // Check if job is actually stopped
            if !matches!(job.state, ProcessState::Stopped(_, _)) {
                let error_msg = format!("bg: job {} is already running", job.job_id);
                debug!("BG_CMD_ALREADY_RUNNING: {}", error_msg);
                ctx.write_stderr(&error_msg)?;
                return Err(anyhow::anyhow!(error_msg));
            }

            let mut job = shell.wait_jobs.remove(index);
            debug!(
                "BG_CMD_JOB_DETAILS: Job details before bg - state: {:?}, pgid: {:?}, pid: {:?}",
                job.state, job.pgid, job.pid
            );

            ctx.write_stdout(&format!(
                "dsh: job {} '{}' to background",
                job.job_id, job.cmd
            ))
            .ok();

            // Set job state to running and send SIGCONT
            let old_state = job.state;
            job.state = ProcessState::Running;
            debug!(
                "BG_CMD_STATE_CHANGE: Set job {} state from {:?} to Running",
                job.job_id, old_state
            );

            // Send SIGCONT to resume the job
            if let Some(pgid) = job.pgid {
                debug!(
                    "BG_CMD_SIGCONT: Sending SIGCONT to process group {} for job {}",
                    pgid, job.job_id
                );
                match killpg(pgid, Signal::SIGCONT) {
                    Ok(_) => {
                        debug!(
                            "BG_CMD_SIGCONT_SUCCESS: SIGCONT sent successfully to job {}",
                            job.job_id
                        );
                    }
                    Err(err) => {
                        error!(
                            "BG_CMD_SIGCONT_ERROR: Failed to send SIGCONT to job {}: {}",
                            job.job_id, err
                        );
                        ctx.write_stderr(&format!("bg: failed to resume job: {err}"))
                            .ok();
                        return Err(err.into());
                    }
                }
            } else {
                warn!(
                    "BG_CMD_NO_PGID: Job {} has no process group ID, cannot send SIGCONT",
                    job.job_id
                );
            }

            // Put the job back in the background jobs list
            shell.wait_jobs.push(job);
            debug!("BG_CMD_SUCCESS: Job moved to background successfully");
        } else {
            let error_msg = if job_spec.is_empty() {
                "bg: no stopped jobs".to_string()
            } else {
                format!("bg: job not found: {job_spec}")
            };
            debug!("BG_CMD_NOT_FOUND: {}", error_msg);
            ctx.write_stderr(&error_msg)?;
            return Err(anyhow::anyhow!(error_msg));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Note: parse_job_spec tests require mock Job structs which depend on process module
    // These tests are kept minimal for now - full tests would require test fixtures
}
