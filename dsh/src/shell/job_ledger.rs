//! Known async PID ledger: wait ownership for background jobs.
//!
//! An active background [`Job`](crate::process::Job) is heavy: it owns child
//! processes, output monitors, and pipe fds. Once it completes, the table
//! must drop that weight — but `wait PID` still needs the final status
//! afterwards. This ledger is the lightweight half of that split: PID,
//! job id, and final status only. No process resources, no monitors.
//!
//! Lifecycle (see `docs/ai/skills/doge-shell-repo/references/invariants/execution.md`):
//!
//! ```text
//! async launch → register(pid) = Active
//! completion observation → mark_completed(pid, status)
//! wait PID → consume_completed(pid) returns the status, entry removed
//! ```
//!
//! Reconciliation (`check_job_state`, `jobs` notices, `fg`/`bg` completion)
//! archives into this ledger but never consumes from it: only an explicit
//! `wait PID` consumes. The ledger is per-`Shell` and never crosses a
//! `ChildShellSnapshot` — a helper inherits the `$!` string value, never
//! the parent's right to wait.
//!
//! Retention is bounded (POSIX `CHILD_MAX` model): the oldest `Completed`
//! entries are pruned first. `Active` ownership is never pruned — dropping
//! a live wait right would orphan a job the shell still owns.

use nix::unistd::Pid;
use std::collections::{HashMap, VecDeque};

/// Fallback retention when `CHILD_MAX` cannot be read from the OS.
const FALLBACK_MAX_RETAINED: usize = 4096;

/// Wait-ownership state of one known async PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnownAsyncState {
    /// Launched and not yet observed as completed.
    Active,
    /// Tree completed; the status is retained until `wait` consumes it.
    Completed { exit_status: i32 },
}

/// One ledger row: metadata and status only, no process resources.
#[derive(Debug, Clone)]
pub(crate) struct KnownAsyncEntry {
    pub pid: Pid,
    pub job_id: usize,
    pub state: KnownAsyncState,
}

/// Bounded `Active → Completed(status)` ledger keyed by raw PID.
#[derive(Debug)]
pub(crate) struct KnownAsyncLedger {
    entries: HashMap<i32, KnownAsyncEntry>,
    /// Registration order (oldest first) for completed-entry pruning.
    order: VecDeque<i32>,
    max_retained: usize,
}

impl KnownAsyncLedger {
    /// Retention from `CHILD_MAX`, with a fixed fallback when the OS value
    /// is unavailable (both Linux and macOS serve `_SC_CHILD_MAX`).
    fn default_limit() -> usize {
        // SAFETY: `sysconf` with `_SC_CHILD_MAX` takes no pointer arguments.
        let max = unsafe { libc::sysconf(libc::_SC_CHILD_MAX) };
        if max > 0 {
            max as usize
        } else {
            FALLBACK_MAX_RETAINED
        }
    }

    /// Test constructor with an explicit retention limit, so unit tests do
    /// not depend on the OS `CHILD_MAX`.
    #[cfg(test)]
    pub(crate) fn with_limit(max_retained: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            max_retained: max_retained.max(1),
        }
    }

    /// Register a freshly launched async job as `Active`.
    ///
    /// A re-registered PID (OS PID reuse while a stale entry survives)
    /// replaces the old entry: the new launch wins, and the order queue
    /// holds no duplicates.
    pub(crate) fn register(&mut self, pid: Pid, job_id: usize) {
        let raw = pid.as_raw();
        if self.entries.contains_key(&raw) {
            self.order.retain(|queued| *queued != raw);
        }
        self.entries.insert(
            raw,
            KnownAsyncEntry {
                pid,
                job_id,
                state: KnownAsyncState::Active,
            },
        );
        self.order.push_back(raw);
        self.prune_completed();
    }

    /// Archive an observed completion. Returns `false` when the PID is
    /// unknown or already completed (no status is invented or overwritten).
    pub(crate) fn mark_completed(&mut self, pid: Pid, exit_status: i32) -> bool {
        let Some(entry) = self.entries.get_mut(&pid.as_raw()) else {
            return false;
        };
        if entry.state != KnownAsyncState::Active {
            return false;
        }
        entry.state = KnownAsyncState::Completed { exit_status };
        // The entry just became prunable: re-apply the bound now instead
        // of waiting for the next `register`. `Active` rows are still
        // never pruned (see `prune_completed`).
        self.prune_completed();
        true
    }

    /// Borrow one entry, if this shell knows the PID at all.
    /// Peek a ledger entry without consuming it.
    ///
    /// Test-only surface until a reconciliation path needs
    /// peek-without-consume (current call sites go through
    /// `consume_completed` directly).
    #[cfg(test)]
    pub(crate) fn entry(&self, pid: Pid) -> Option<&KnownAsyncEntry> {
        self.entries.get(&pid.as_raw())
    }

    /// Borrow the entry for `pid`, but only while this shell still holds
    /// `Active` wait ownership of it. Detach decisions (`job_exit`) must
    /// never treat a retained `Completed` status as a live child.
    pub(crate) fn active_entry(&self, pid: Pid) -> Option<&KnownAsyncEntry> {
        let entry = self.entries.get(&pid.as_raw())?;
        (entry.state == KnownAsyncState::Active).then_some(entry)
    }

    /// Find a PID by its job id (for completion paths that own a `Job`
    /// but have not resolved its canonical PID yet).
    pub(crate) fn entry_by_job_id(&self, job_id: usize) -> Option<&KnownAsyncEntry> {
        self.entries.values().find(|entry| entry.job_id == job_id)
    }

    /// The canonical PID for a stable job number, if this shell knows it.
    ///
    /// Intention-revealing query for `%N` resolution: explicit job numbers
    /// outlive the heavy `Job` (reconciliation archives the status here),
    /// so `wait %N` consults this after the active table misses. Peek only —
    /// status still leaves through [`consume_completed`](Self::consume_completed).
    pub(crate) fn pid_by_job_id(&self, job_id: usize) -> Option<Pid> {
        self.entry_by_job_id(job_id).map(|entry| entry.pid)
    }

    /// The retained status for a completed PID, without consuming it.
    ///
    /// Peek only: `wait -n` scans every target for an already-completed
    /// status first and consumes exactly the one it selects.
    pub(crate) fn completed_status(&self, pid: Pid) -> Option<i32> {
        match self.entries.get(&pid.as_raw())?.state {
            KnownAsyncState::Completed { exit_status } => Some(exit_status),
            KnownAsyncState::Active => None,
        }
    }

    /// The stable job number for a known PID, if this shell knows it.
    ///
    /// Fills `WaitCompletion` metadata for PID-derived targets so `wait -p`
    /// reports the canonical associated PID without a new lookup path.
    pub(crate) fn job_id_for_pid(&self, pid: Pid) -> Option<usize> {
        self.entries.get(&pid.as_raw()).map(|entry| entry.job_id)
    }

    /// Consume a retained status: returns it and removes the entry.
    /// Returns `None` for unknown PIDs and for still-`Active` ones — only
    /// `Completed` rows are consumable.
    pub(crate) fn consume_completed(&mut self, pid: Pid) -> Option<i32> {
        let raw = pid.as_raw();
        match self.entries.get(&raw)?.state {
            KnownAsyncState::Completed { exit_status } => {
                self.entries.remove(&raw);
                self.order.retain(|queued| *queued != raw);
                Some(exit_status)
            }
            KnownAsyncState::Active => None,
        }
    }

    /// Every PID this shell may wait for, oldest first.
    pub(crate) fn known_pids(&self) -> Vec<Pid> {
        self.order
            .iter()
            .filter_map(|raw| self.entries.get(raw).map(|entry| entry.pid))
            .collect()
    }

    /// Drop one entry unconditionally (only for paths that already moved
    /// the job's ownership elsewhere, e.g. PID-reuse replacement is handled
    /// by [`register`](Self::register) instead).
    pub(crate) fn remove(&mut self, pid: Pid) -> Option<KnownAsyncEntry> {
        let raw = pid.as_raw();
        self.order.retain(|queued| *queued != raw);
        self.entries.remove(&raw)
    }

    /// Enforce the bound: evict oldest `Completed` entries first. `Active`
    /// ownership is never pruned, even over the limit.
    fn prune_completed(&mut self) {
        while self.entries.len() > self.max_retained
            && let Some(victim) = self
                .order
                .iter()
                .find(|raw| {
                    matches!(
                        self.entries.get(*raw).map(|entry| entry.state),
                        Some(KnownAsyncState::Completed { .. })
                    )
                })
                .copied()
        {
            self.entries.remove(&victim);
            self.order.retain(|queued| *queued != victim);
        }
    }
}

impl Default for KnownAsyncLedger {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            max_retained: Self::default_limit(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(raw: i32) -> Pid {
        Pid::from_raw(raw)
    }

    #[test]
    fn register_marks_active() {
        let mut ledger = KnownAsyncLedger::with_limit(8);
        ledger.register(pid(101), 1);
        assert_eq!(
            ledger.entry(pid(101)).map(|entry| entry.state),
            Some(KnownAsyncState::Active)
        );
        assert_eq!(ledger.entry(pid(101)).map(|entry| entry.job_id), Some(1));
    }

    #[test]
    fn completed_status_retained_until_consumed() {
        let mut ledger = KnownAsyncLedger::with_limit(8);
        ledger.register(pid(101), 1);
        assert!(ledger.mark_completed(pid(101), 7));
        assert_eq!(
            ledger.entry(pid(101)).map(|entry| entry.state),
            Some(KnownAsyncState::Completed { exit_status: 7 })
        );
        assert_eq!(ledger.consume_completed(pid(101)), Some(7));
        assert!(ledger.entry(pid(101)).is_none());
    }

    #[test]
    fn unknown_consume_returns_none() {
        let mut ledger = KnownAsyncLedger::with_limit(8);
        assert_eq!(ledger.consume_completed(pid(999)), None);
        ledger.register(pid(101), 1);
        // Active rows are not consumable.
        assert_eq!(ledger.consume_completed(pid(101)), None);
        assert!(ledger.entry(pid(101)).is_some());
    }

    #[test]
    fn multiple_pids_retained_independently() {
        let mut ledger = KnownAsyncLedger::with_limit(8);
        ledger.register(pid(101), 1);
        ledger.register(pid(102), 2);
        assert!(ledger.mark_completed(pid(101), 3));
        assert!(ledger.mark_completed(pid(102), 7));
        assert_eq!(ledger.consume_completed(pid(101)), Some(3));
        assert_eq!(ledger.consume_completed(pid(102)), Some(7));
    }

    #[test]
    fn pid_reuse_replaces_stale_entry() {
        let mut ledger = KnownAsyncLedger::with_limit(8);
        ledger.register(pid(101), 1);
        assert!(ledger.mark_completed(pid(101), 3));
        // The OS recycled PID 101 for a new async job: the new launch wins.
        ledger.register(pid(101), 2);
        assert_eq!(
            ledger.entry(pid(101)).map(|entry| entry.state),
            Some(KnownAsyncState::Active)
        );
        assert_eq!(ledger.entry(pid(101)).map(|entry| entry.job_id), Some(2));
        assert_eq!(
            ledger
                .known_pids()
                .iter()
                .filter(|p| **p == pid(101))
                .count(),
            1,
            "order queue must not hold duplicates"
        );
    }

    #[test]
    fn bounded_pruning_drops_oldest_completed_first() {
        let mut ledger = KnownAsyncLedger::with_limit(2);
        ledger.register(pid(101), 1);
        ledger.register(pid(102), 2);
        assert!(ledger.mark_completed(pid(101), 1));
        assert!(ledger.mark_completed(pid(102), 2));
        ledger.register(pid(103), 3);
        assert!(
            ledger.entry(pid(101)).is_none(),
            "oldest completed entry is pruned past the limit"
        );
        assert!(ledger.entry(pid(102)).is_some());
        assert!(ledger.entry(pid(103)).is_some());
    }

    #[test]
    fn active_entries_survive_completed_pruning() {
        let mut ledger = KnownAsyncLedger::with_limit(2);
        ledger.register(pid(101), 1);
        ledger.register(pid(102), 2);
        assert!(ledger.mark_completed(pid(101), 1));
        // 102 is still active: pruning must evict 101, never 102.
        ledger.register(pid(103), 3);
        assert!(ledger.entry(pid(102)).is_some());
        assert_eq!(
            ledger.entry(pid(102)).map(|entry| entry.state),
            Some(KnownAsyncState::Active)
        );
    }

    #[test]
    fn active_overflow_is_pruned_when_one_becomes_completed() {
        let mut ledger = KnownAsyncLedger::with_limit(1);
        ledger.register(pid(101), 1);
        ledger.register(pid(102), 2);
        // Both rows are `Active` ownership: neither may be pruned yet.
        assert!(ledger.entry(pid(101)).is_some());
        assert!(ledger.entry(pid(102)).is_some());
        // Completing 101 makes it prunable immediately — the bound is
        // re-applied here, not on the next `register`.
        assert!(ledger.mark_completed(pid(101), 3));
        assert!(
            ledger.entry(pid(101)).is_none(),
            "completed entry must be pruned as soon as the bound can apply"
        );
        assert_eq!(
            ledger.entry(pid(102)).map(|entry| entry.state),
            Some(KnownAsyncState::Active)
        );
    }

    #[test]
    fn active_only_overflow_keeps_ownership() {
        let mut ledger = KnownAsyncLedger::with_limit(1);
        ledger.register(pid(101), 1);
        ledger.register(pid(102), 2);
        // No completed entry exists to prune: both live jobs stay known.
        assert!(ledger.entry(pid(101)).is_some());
        assert!(ledger.entry(pid(102)).is_some());
    }

    #[test]
    fn known_pids_lists_in_registration_order() {
        let mut ledger = KnownAsyncLedger::with_limit(8);
        ledger.register(pid(102), 2);
        ledger.register(pid(101), 1);
        assert_eq!(ledger.known_pids(), vec![pid(102), pid(101)]);
    }
}
