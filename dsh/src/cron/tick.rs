//! The one claim-and-spawn cycle both drivers share.
//!
//! An external `cron tick` and the in-session runner ask the same question —
//! "what is due right now" — and must get disjoint answers when they ask at
//! the same instant. They get that by sharing this exact function rather than
//! each re-implementing the claim; see `store::claim`'s doc comment for how
//! the claim itself stays safe across processes.

use crate::cron::exec::spawn_run_child;
use crate::cron::store::SqliteCronStore;
use anyhow::Result;
use dsh_builtin::shell_capabilities::CronStore;
use dsh_types::cron::job::{RunOutcome, RunReason, RunState, RunTrigger};

/// Ceiling on jobs started by one tick, so a burst of simultaneously-due work
/// cannot fork the machine to a standstill. Each is its own process, so this
/// is a much looser limit than `sched`'s in-process `MAX_PARALLEL`.
pub const MAX_PER_TICK: usize = 16;

pub struct TickReport {
    /// Names of the jobs a child was started for.
    pub started: Vec<String>,
    /// Job/error pairs where even starting the child failed.
    pub spawn_failed: Vec<(String, String)>,
}

/// An id unique to this process, for the `claimed_by` column.
///
/// Not cryptographically unique — it does not need to be. It only has to
/// avoid colliding with another live process closely enough that a lease
/// reap's same-host liveness check (`kill(pid, 0)`) means something.
pub fn owner_id() -> String {
    let host = nix::unistd::gethostname()
        .ok()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown-host".to_string());
    format!("{host}:{}:{}", std::process::id(), xid::new())
}

/// Claims up to `limit` due jobs and starts a detached child for each.
///
/// A run whose *child could not even be started* is completed on the spot as
/// `Failed{Spawn}` — leaving it `queued` forever would make it look like it
/// is still about to happen.
pub fn run_once(
    store: &SqliteCronStore,
    owner: &str,
    limit: usize,
    trigger: RunTrigger,
    now: i64,
) -> Result<TickReport> {
    store.reap_expired_leases(now)?;
    let claims = store.claim_due(now, owner, limit, trigger)?;

    let mut started = Vec::new();
    let mut spawn_failed = Vec::new();
    for claim in claims {
        match spawn_run_child(&claim.run_id) {
            Ok(mut child) => {
                started.push(claim.job_name);
                // `Child` is not reaped on drop, and the in-session driver
                // that calls `run_once` in a loop (`runner.rs`) lives for the
                // whole session - without this, every completed run left a
                // zombie behind until the session itself exited. A dedicated
                // thread just to collect the exit status keeps this call
                // itself non-blocking (the run is meant to outlive whoever
                // ticked it).
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => {
                spawn_failed.push((claim.job_name, error.to_string()));
                let outcome = RunOutcome {
                    state: RunState::Failed,
                    reason: Some(RunReason::Spawn),
                    stderr: error.to_string(),
                    digest: None,
                    ..Default::default()
                };
                // Best-effort: if even this write fails, the lease will still
                // expire and the next scan reaps it as `LeaseLost`.
                let _ = store.complete(&claim.run_id, &outcome, now);
            }
        }
    }
    Ok(TickReport {
        started,
        spawn_failed,
    })
}

#[cfg(test)]
mod tests;
