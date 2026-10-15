//! Poll only this task's jobs; never hold the jobs lock while waiting.
use super::*;
use dsh_builtin::agent::jobs::MAX_JOB_WAIT_MS;

pub(super) fn owned_job(name: &str, arguments: &str, shared: &Shared) -> Result<String, String> {
    let args: Value = serde_json::from_str(arguments).map_err(|_| "invalid owned-job arguments")?;
    let id = args["job_id"].as_str().ok_or("owned job_id required")?;
    let offset = args["offset"].as_u64().unwrap_or(0).min(usize::MAX as u64) as usize;
    let limit = args["limit"].as_u64().unwrap_or(4096).min(65536) as usize;
    let wait = Duration::from_millis(args["wait_ms"].as_u64().unwrap_or(0).min(MAX_JOB_WAIT_MS));
    let started = Instant::now();
    loop {
        shared.cancel_disconnected();
        {
            let mut inner = shared.inner.lock();
            inner.task.check_deadline(shared.elapsed());
            if !active(inner.task.status()) || shared.canceled.load(Ordering::Acquire) {
                return Err("background task stopped".into());
            }
        }
        let snapshot = {
            let mut jobs = shared.jobs.lock();
            // Lookup also precedes cancellation; no access to foreground jobs.
            jobs.snapshot(id, 0, 0).map_err(|e| e.to_string())?;
            if name == "job_cancel" {
                jobs.cancel(id).map_err(|e| e.to_string())?;
            }
            jobs.snapshot(id, offset, limit)
                .map_err(|e| e.to_string())?
        };
        if name == "job_cancel" || snapshot["status"] != "running" || started.elapsed() >= wait {
            return Ok(snapshot.to_string());
        }
        let remaining = wait
            .saturating_sub(started.elapsed())
            .min(shared.time_left());
        // Condvar permits prompt cancellation/restriction. The 50ms ceiling also
        // notices worker EOF and job exit without consuming their protocol/wait.
        let mut inner = shared.inner.lock();
        if !active(inner.task.status()) {
            continue;
        }
        shared
            .changed
            .wait_for(&mut inner, remaining.min(Duration::from_millis(50)));
    }
}
