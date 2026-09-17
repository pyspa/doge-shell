//! The backstop that keeps a hung agent task from running forever.
//!
//! `AgentTask.time_budget_ms` is a *cooperative* budget - `run_task` checks it
//! between tool calls, so a call that never returns (a stuck HTTP request, a
//! tool that blocks forever) is never checked at all. An unattended run (a
//! cron AI job, or `agent run --detach`) has no outer process watching it:
//! `run_task` runs synchronously, in the same process the watchdog thread
//! lives in. So the deadline has to live here, as a thread that outlives
//! nothing it does not have to.
//!
//! Shared by cron (`dsh/src/cron/run_job.rs`, whose deadline is tied to its
//! lease so a killed run does not also get reclaimed as abandoned) and the
//! detached agent (`dsh/src/agent/detach.rs`, which has no lease and instead
//! adds a fixed grace period) - only the deadline differs; arming and firing
//! do not.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Starts a thread that kills this process's entire process group after
/// `deadline_secs`, unless the returned flag is cleared first.
///
/// Returns the flag the caller must clear once the watched work has returned
/// on its own; the watchdog thread checks it once, after waking, and does
/// nothing at all once it is cleared.
pub(crate) fn arm(deadline_secs: u64) -> Arc<AtomicBool> {
    let armed = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&armed);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(deadline_secs.max(1)));
        if flag.load(Ordering::SeqCst) {
            // This process was made its own process group leader by
            // whichever spawned it (`crate::detached_child::spawn`), so pgid
            // 0 (POSIX: "the sender's own process group") takes down this
            // process and anything it started. The run's row is left
            // `Running`; the caller's own recovery (`recover_interrupted`
            // for a detached agent task, `reap_expired_leases` for cron)
            // closes it out on the next scan, exactly as it already does for
            // any process that died without reporting back.
            unsafe {
                libc::killpg(0, libc::SIGKILL);
            }
        }
    });
    armed
}

/// Clears the flag [`arm`] returned, telling the watchdog thread the work it
/// was guarding finished on its own.
pub(crate) fn disarm(armed: &Arc<AtomicBool>) {
    armed.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disarming_before_the_deadline_leaves_the_process_alone() {
        let armed = arm(60);
        disarm(&armed);
        assert!(!armed.load(Ordering::SeqCst));
    }
}
