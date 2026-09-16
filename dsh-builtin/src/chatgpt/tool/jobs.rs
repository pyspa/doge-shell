//! `job_status` / `job_output` / `job_cancel`, for both entry points.
//!
//! A task's jobs live in its `AgentRuntime` and are archived to SQLite; an
//! interactive turn's live in [`crate::chatgpt::jobs`] and are archived in
//! memory. Everything above this - the tool names, the arguments, the shape of
//! the answer - is the same either way, because the model should not have to
//! know which entry point it is running under.
use super::*;
use serde_json::json;
use std::time::{Duration, Instant};

/// Ceiling on the server-side wait one poll may ask for.
const MAX_WAIT_MS: u64 = 60_000;
/// How often a wait re-checks the job.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn dispatch(
    name: &str,
    args: &Value,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    let id = args["job_id"]
        .as_str()
        .ok_or("job_id required")?
        .to_string();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(4096) as usize;
    let wait = Duration::from_millis(args["wait_ms"].as_u64().unwrap_or(0).min(MAX_WAIT_MS));

    if name == "job_cancel" {
        cancel(&id, proxy)?;
    } else if !wait.is_zero() {
        wait_for_change(&id, wait, proxy);
    }

    let mut result = snapshot(&id, offset, limit, name == "job_cancel", proxy)?;

    // Only the interactive registry rate-limits: a task has no `JobMeta` to
    // count polls with, and the instruction was to leave its behaviour alone.
    if proxy.agent_runtime().is_none() {
        // A job that outlived its `execute` wait window still has a user
        // watching: without this its output stops the moment the model was
        // handed a handle, and the rest of the build never reaches the screen.
        crate::chatgpt::jobs::echo_pending(&id);

        let still_running = result["status"] == "running";
        if still_running && name != "job_cancel" {
            let slept = back_off(&id, proxy);
            if !slept.is_zero() {
                result = snapshot(&id, offset, limit, false, proxy)?;
                result["polled_after_ms"] = json!(slept.as_millis() as u64);
            }
        }
        crate::chatgpt::jobs::with(|jobs| jobs.note_poll(&id, still_running));
    }

    Ok(result.to_string())
}

/// Sleep off the floor a repeat poll of a still-running job owes.
///
/// One poll is one API round trip. Without a floor a model that decides to
/// watch a build spends the turn's whole token budget asking "is it done yet"
/// as fast as the provider will answer.
fn back_off(id: &str, proxy: &mut dyn ChatToolHost) -> Duration {
    let floor = crate::chatgpt::jobs::with(|jobs| jobs.poll_backoff(id));
    if floor.is_zero() {
        return Duration::ZERO;
    }

    let started = Instant::now();
    while started.elapsed() < floor {
        if crate::chatgpt::task_cancelled(proxy) {
            break;
        }
        std::thread::sleep(WAIT_POLL_INTERVAL.min(floor - started.elapsed()));
    }
    started.elapsed()
}

/// Wait for the job to leave `running`, or for `wait` to run out.
///
/// Opt-in (`wait_ms`, default 0), so nothing changes for a caller that does
/// not ask. When it is asked for, one round trip covers the whole wait instead
/// of one per poll.
fn wait_for_change(id: &str, wait: Duration, proxy: &mut dyn ChatToolHost) {
    let interactive = proxy.agent_runtime().is_none();
    let started = Instant::now();
    while started.elapsed() < wait {
        if crate::chatgpt::task_cancelled(proxy) {
            return;
        }
        if interactive {
            crate::chatgpt::jobs::echo_pending(id);
        }
        let running = snapshot(id, 0, 0, false, proxy)
            .map(|state| state["status"] == "running")
            .unwrap_or(false);
        if !running {
            return;
        }
        std::thread::sleep(WAIT_POLL_INTERVAL);
    }
}

fn cancel(id: &str, proxy: &mut dyn ChatToolHost) -> Result<(), String> {
    match proxy.agent_runtime() {
        Some(runtime) => runtime.lock().jobs.cancel(id).map_err(|e| e.to_string()),
        None => crate::chatgpt::jobs::with(|jobs| jobs.cancel(id)).map_err(|e| e.to_string()),
    }
}

/// The job's state, falling back to the archive for one already reaped.
///
/// `job_cancel` does not fall back: cancelling a job that is no longer there
/// is a mistake worth reporting, while reading one is not.
fn snapshot(
    id: &str,
    offset: usize,
    limit: usize,
    live_only: bool,
    proxy: &mut dyn ChatToolHost,
) -> Result<Value, String> {
    match proxy.agent_runtime() {
        Some(runtime) => {
            let runtime = runtime.lock();
            runtime
                .jobs
                .snapshot(id, offset, limit)
                .or_else(|error| {
                    if live_only {
                        return Err(error);
                    }
                    let mut archived = runtime.store.load_artifact(&runtime.task.id, id)?;
                    archived["archived"] = json!(true);
                    archived["job_id"] = json!(id);
                    for stream in ["stdout", "stderr"] {
                        archived[stream] = json!(window(
                            archived[stream].as_str().unwrap_or_default(),
                            offset,
                            limit
                        ));
                    }
                    Ok(archived)
                })
                .map_err(|e| e.to_string())
        }
        None => crate::chatgpt::jobs::with(|jobs| {
            jobs.snapshot(id, offset, limit).or_else(|error| {
                if live_only {
                    return Err(error);
                }
                let mut archived = jobs
                    .archived(id)
                    .ok_or_else(|| anyhow::anyhow!("unknown job"))?;
                for stream in ["stdout", "stderr"] {
                    archived[stream] = json!(window(
                        archived[stream].as_str().unwrap_or_default(),
                        offset,
                        limit
                    ));
                }
                Ok(archived)
            })
        })
        .map_err(|e| e.to_string()),
    }
}

/// The same paged window `AgentJobs::snapshot` applies, for archived text.
fn window(text: &str, offset: usize, limit: usize) -> &str {
    let start = text.ceil_char_boundary(offset.min(text.len()));
    let end = text.floor_char_boundary(start.saturating_add(limit.min(65536)).min(text.len()));
    &text[start..end]
}
