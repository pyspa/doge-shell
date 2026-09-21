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
