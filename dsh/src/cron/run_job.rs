//! Executing one claimed run, inside its own `dsh -c "cron run-job <uuid>"`.
//!
//! This is the far side of the process boundary described in the module doc
//! one level up. The parent handed over a UUID and nothing else; everything
//! this run needs is read back from the store here, so a job's command
//! never passes through an extra shell parser.

use anyhow::Result;
use dsh_builtin::config_paths;
use dsh_builtin::shell_capabilities::CronStore;
use dsh_types::cron::job::{ClaimedRun, JobKind, RunOutcome, RunReason, RunState};
use dsh_types::safety_policy::redact_sensitive_text;
use std::time::Instant;

use super::exec;
use super::store::SqliteCronStore;
use crate::shell::Shell;

/// Runs the claimed run named by `run_id` and records what happened.
///
/// Every failure inside becomes a recorded outcome rather than an error: a run
/// that propagated its error would stay `running` in the store forever, and
/// the next scan would wait out its whole lease before anyone found out.
pub fn execute(_shell: &mut Shell, _ctx: &dsh_types::Context, run_id: &str) -> Result<()> {
    let store = SqliteCronStore::open(&config_paths::cron_state_dir())?;
    let run = store.start(run_id, chrono::Utc::now().timestamp())?;
    let started = Instant::now();

    let outcome = match run.kind {
        JobKind::Sh => shell_outcome(&run),
        JobKind::Ai => stopped(
            RunState::Failed,
            RunReason::Config,
            "agent jobs are no longer supported; recreate this job as a shell command",
            started,
        ),
    };

    store.complete(run_id, &outcome, chrono::Utc::now().timestamp())
}

fn shell_outcome(run: &ClaimedRun) -> RunOutcome {
    let result = exec::run_command(run);
    let state = if result.exit_code == 0 && !result.timed_out {
        RunState::Succeeded
    } else {
        RunState::Failed
    };
    RunOutcome {
        state,
        reason: result.timed_out.then_some(RunReason::Timeout),
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        duration_ms: result.duration.as_millis() as u64,
        // The store keeps these; a command that printed a token would leave it
        // on disk for as long as the history does.
        stdout: redact_sensitive_text(&result.stdout),
        stderr: redact_sensitive_text(&result.stderr),
        digest: Some(exec::digest(&result.stdout)),
        ..Default::default()
    }
}

/// A finished-without-running outcome, carrying why.
fn stopped(state: RunState, reason: RunReason, detail: &str, started: Instant) -> RunOutcome {
    RunOutcome {
        state,
        reason: Some(reason),
        exit_code: if state == RunState::Succeeded { 0 } else { 1 },
        duration_ms: started.elapsed().as_millis() as u64,
        stderr: redact_sensitive_text(detail),
        // No digest: a run that produced no output must not overwrite what the
        // next `--on change` comparison is made against.
        digest: None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
