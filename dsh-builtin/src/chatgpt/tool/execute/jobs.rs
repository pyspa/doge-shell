//! The interactive half of `execute`: start the command as a managed job,
//! show it while it runs, and either answer with the finished result or hand
//! the model a handle to poll.
//!
//! An agent task has always run commands this way. Doing the same for `!`
//! means one capture engine instead of two, a `cargo build` that is not killed
//! at two minutes, and output on the screen while it is produced rather than
//! only once it is over.
//!
//! What does *not* change: a command that finishes inside the wait window
//! returns exactly what it returned before - `render_result`'s
//! `{exit_code, stdout, stderr}`, and the same bytes echoed to the terminal.
use super::*;
use crate::chatgpt::jobs as chat_jobs;
use crate::chatgpt::ui::SpinnerGuard;

/// How often the wait loop re-checks the job.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Run `command` as a managed job, waiting up to `yield_ms` for it to finish.
pub(super) fn run_as_job(
    command: &str,
    cwd: Option<&Path>,
    timeout: Duration,
    yield_ms: u64,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    let builder = capture::shell_command(command, cwd);
    let id = chat_jobs::start(builder, command, cwd.map(Path::to_path_buf), timeout)
        .map_err(|err| format!("chat: failed to execute `{command}`: {err}"))?;

    let outcome = wait_for_job(&id, command, yield_ms, proxy);

    match outcome {
        Wait::Finished { cancelled } => Ok(finished_result(&id, timeout, cancelled)),
        Wait::StillRunning => Ok(still_running_result(&id, command)),
    }
}

enum Wait {
    Finished { cancelled: bool },
    StillRunning,
}

/// Watch the job for up to `yield_ms`, echoing what it writes.
///
/// No spinner is running here: `SpinnerGuard` only lives for the duration of a
/// provider request, and tool calls happen outside that scope - so this one
/// owns the bottom line for as long as it waits.
fn wait_for_job(id: &str, command: &str, yield_ms: u64, proxy: &mut dyn ChatToolHost) -> Wait {
    let spinner = SpinnerGuard::start("");
    let started = Instant::now();
    let label = command.split_whitespace().collect::<Vec<_>>().join(" ");

    loop {
        if crate::chatgpt::task_cancelled(proxy) {
            chat_jobs::with(|jobs| jobs.cancel(id)).ok();
            echo(&spinner, id);
            return Wait::Finished { cancelled: true };
        }

        echo(&spinner, id);

        let running = chat_jobs::with(|jobs| jobs.snapshot(id, 0, 0))
            .map(|state| state["status"] == "running")
            .unwrap_or(false);
        if !running {
            // Once more: the worker only publishes its final state after both
            // streams reach EOF, so bytes written between the echo above and
            // this read are in the ring but have not been on the screen.
            echo(&spinner, id);
            return Wait::Finished { cancelled: false };
        }
        if started.elapsed().as_millis() >= u128::from(yield_ms) {
            return Wait::StillRunning;
        }

        spinner.set_tail(&format!("{}s · {label}", started.elapsed().as_secs()));
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// `indicatif` owns the bottom line while it ticks, so the write has to happen
/// inside `suspend`.
fn echo(spinner: &SpinnerGuard, id: &str) {
    spinner.suspend(|| chat_jobs::echo_pending(id));
}

/// The same shape the synchronous path returned, from the job's own log.
fn finished_result(id: &str, timeout: Duration, cancelled: bool) -> String {
    let Ok(state) = chat_jobs::with(|jobs| jobs.snapshot(id, 0, 0)) else {
        return render_result(-1, "", "", Some("the job could not be read back".into()));
    };
    // Not `snapshot`: its window is capped and its text is masked for the
    // model, while `render_result` wants the whole log and does its own
    // middle-truncation. Redaction happens inside `render_result`'s caller
    // chain - here, explicitly, before the text leaves this function.
    let Ok(raw) = chat_jobs::with(|jobs| jobs.read_raw(id, 0, 0)) else {
        return render_result(-1, "", "", Some("the job could not be read back".into()));
    };

    let stdout = redact(&raw.stdout);
    let stderr = redact(&raw.stderr);
    let exit_code = state["exit_code"].as_i64().unwrap_or(-1) as i32;

    let note = if cancelled || state["status"] == "cancelled" {
        Some("command cancelled; output may be partial".to_string())
    } else if state["status"] == "timed_out" {
        Some(format!(
            "command exceeded timeout_ms={} and was killed; output below is partial",
            timeout.as_millis()
        ))
    } else if !raw.stdout_complete || !raw.stderr_complete {
        // Saying nothing here would present a truncated capture as the whole
        // output, which is exactly the mistake the timeout note exists to avoid.
        Some(
            "output capture stopped early; a background process still holds the pipe, so the \
             output below may be incomplete"
                .to_string(),
        )
    } else {
        None
    };

    render_result(exit_code, &stdout, &stderr, note)
}

/// The handle the model polls, plus one line telling the user it exists.
fn still_running_result(id: &str, command: &str) -> String {
    eprintln!(
        "\x1b[2mjob {} still running ({command}); the assistant can follow it with job_status\x1b[0m",
        chat_jobs::short_id(id)
    );

    chat_jobs::with(|jobs| jobs.snapshot(id, 0, 4096))
        .map(|state| state.to_string())
        .unwrap_or_else(|err| format!("Error: {err}"))
}

fn redact(bytes: &[u8]) -> String {
    dsh_types::safety_policy::redact_sensitive_text(&String::from_utf8_lossy(bytes))
}
