use super::*;
use std::sync::Mutex as StdMutex;

/// Records every call it receives instead of touching anything external, so
/// these tests never need a real `herdr` binary. Also tracks the same
/// "last confirmed delivery" `herdr::HerdrReporter` does, so
/// `reconcile_if_stale` behaves the same way against this fake as it would
/// against the real backend.
#[derive(Default)]
struct RecordingReporter {
    calls: StdMutex<Vec<Call>>,
    delivered: StdMutex<Option<AgentState>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Report(AgentState, u64),
    Release(u64),
}

impl LifecycleReporter for RecordingReporter {
    fn report(&self, state: &AgentState, seq: u64) {
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Call::Report(state.clone(), seq));
        *self.delivered.lock().unwrap_or_else(|p| p.into_inner()) = Some(state.clone());
    }

    fn release(&self, seq: u64) {
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Call::Release(seq));
        *self.delivered.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn shutdown(&self, _timeout: Duration) {}

    fn is_stale(&self, desired: &AgentState) -> bool {
        self.delivered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            != Some(desired)
    }
}

fn manager_with_recorder() -> (Arc<AgentLifecycleManager>, Arc<RecordingReporter>) {
    let reporter = Arc::new(RecordingReporter::default());
    let manager = AgentLifecycleManager::new(reporter.clone());
    (manager, reporter)
}

fn calls_of(reporter: &RecordingReporter) -> Vec<Call> {
    reporter
        .calls
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

#[test]
fn null_reporter_is_a_pure_no_op() {
    let manager = AgentLifecycleManager::null();
    manager.report_working();
    manager.report_blocked("anything");
    manager.report_idle();
    manager.shutdown();
    manager.reconcile_if_stale();
    // Nothing to assert beyond "did not panic and did not touch anything
    // external" - that is the whole point of `NullReporter`.
}

#[test]
fn repeated_state_is_deduplicated() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    manager.report_working();
    manager.report_working();
    let calls = calls_of(&reporter);
    assert_eq!(
        calls.len(),
        1,
        "repeats of the same state should collapse: {calls:?}"
    );
}

#[test]
fn changed_blocked_reason_is_not_deduplicated() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_blocked("first reason");
    manager.report_blocked("second reason");
    let calls = calls_of(&reporter);
    assert_eq!(
        calls.len(),
        2,
        "a new blocked reason is worth re-surfacing: {calls:?}"
    );
}

#[test]
fn sequence_numbers_are_strictly_increasing() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    manager.report_blocked("waiting on approval");
    manager.report_working();
    manager.report_idle();
    let calls = calls_of(&reporter);
    let seqs: Vec<u64> = calls
        .iter()
        .map(|c| match c {
            Call::Report(_, seq) => *seq,
            Call::Release(seq) => *seq,
        })
        .collect();
    for pair in seqs.windows(2) {
        assert!(pair[1] > pair[0], "seq must strictly increase: {seqs:?}");
    }
}

#[test]
fn a_later_process_never_reuses_a_seq_from_an_earlier_one() {
    // Simulates a `dsh` process restart in the same pane: Herdr's high-water
    // mark is per-source and is not documented to reset on `release-agent`,
    // so a fresh counter starting at 0/1 would have every report silently
    // ignored as stale. Seeding from wall-clock time is what prevents that -
    // modeled here with an explicit seed rather than a real clock, since two
    // `now_millis()`-seeded managers built microseconds apart in the same
    // test process can otherwise land in the same millisecond and make this
    // assertion flaky for a reason that has nothing to do with the behavior
    // under test.
    let seed_from_a_prior_process = now_millis() + 10_000; // as if far in this process's future
    let reporter = Arc::new(RecordingReporter::default());
    let manager =
        AgentLifecycleManager::new_with_seed_seq(reporter.clone(), seed_from_a_prior_process);
    manager.report_working();
    let first_seq = match calls_of(&reporter).first().unwrap() {
        Call::Report(_, seq) => *seq,
        Call::Release(seq) => *seq,
    };
    assert!(
        first_seq > seed_from_a_prior_process,
        "a fresh manager's first seq ({first_seq}) must exceed whatever it was seeded with \
         ({seed_from_a_prior_process}), never fall back to a raw clock reading below it"
    );
}

#[test]
fn owner_pid_guard_makes_a_foreign_process_manager_inert() {
    let reporter = Arc::new(RecordingReporter::default());
    // Build a manager and then pretend it belongs to a different process
    // (as a forked child would see, holding a pre-fork clone) by
    // constructing it directly rather than through `new`.
    let manager = Arc::new(AgentLifecycleManager {
        owner_pid: std::process::id().wrapping_add(1),
        state: Mutex::new(ManagerState {
            last_reported: None,
            last_seq: 0,
            yield_depth: 0,
        }),
        depth: AtomicUsize::new(0),
        reporter: reporter.clone(),
        shutdown_once: Once::new(),
    });
    manager.report_working();
    manager.report_blocked("should never be sent");
    manager.shutdown();
    assert!(
        calls_of(&reporter).is_empty(),
        "a manager owned by a different pid must not report or release"
    );
}

#[test]
fn nested_turns_only_report_idle_when_the_outermost_ends() {
    let (manager, reporter) = manager_with_recorder();
    let outer = manager.begin_turn();
    let inner = manager.begin_turn();
    drop(inner);
    assert!(
        !calls_of(&reporter)
            .iter()
            .any(|c| matches!(c, Call::Report(AgentState::Idle, _))),
        "the inner guard must not end the turn"
    );
    drop(outer);
    assert!(
        calls_of(&reporter)
            .iter()
            .any(|c| matches!(c, Call::Report(AgentState::Idle, _))),
        "the outer guard must end the turn"
    );
}

#[test]
fn a_turn_that_ends_blocked_is_not_overwritten_with_idle() {
    let (manager, reporter) = manager_with_recorder();
    let turn = manager.begin_turn();
    // The persistent-task path: the turn function returns normally after
    // setting `TaskStatus::InputRequired`, and the caller reports `blocked`
    // explicitly before the guard goes out of scope.
    manager.report_blocked("task needs approval; nobody is watching");
    drop(turn);
    let calls = calls_of(&reporter);
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, Call::Report(AgentState::Idle, _))),
        "ending a turn that is blocked must not report idle: {calls:?}"
    );
    // A later, genuinely new turn still reports idle normally.
    drop(manager.begin_turn());
    assert!(
        calls_of(&reporter)
            .iter()
            .any(|c| matches!(c, Call::Report(AgentState::Idle, _))),
        "a subsequent turn must still be able to end idle"
    );
}

#[test]
fn interactive_blocked_bracket_returns_to_working_not_idle() {
    let (manager, reporter) = manager_with_recorder();
    let turn = manager.begin_turn();
    let blocked = manager.begin_blocked("delete /etc/hosts?");
    drop(blocked);
    let calls = calls_of(&reporter);
    assert!(
        matches!(calls.last(), Some(Call::Report(AgentState::Working, _))),
        "the interactive approval bracket must resolve back to Working, not Idle: {calls:?}"
    );
    drop(turn);
}

#[test]
fn shutdown_releases_exactly_once() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    manager.shutdown();
    manager.shutdown();
    manager.shutdown();
    let release_count = calls_of(&reporter)
        .iter()
        .filter(|c| matches!(c, Call::Release(_)))
        .count();
    assert_eq!(release_count, 1, "shutdown must be idempotent");
}

#[test]
fn message_sanitization_collapses_whitespace_and_truncates() {
    let long_reason = "line one\nline two\n\n".to_string() + &"x".repeat(MAX_MESSAGE_CHARS + 50);
    let sanitized = sanitize_message(&long_reason);
    assert!(!sanitized.contains('\n'));
    assert!(sanitized.chars().count() <= MAX_MESSAGE_CHARS + 1); // +1 for the "…" marker
}

#[test]
fn the_very_first_report_is_never_deduplicated_away() {
    // A fresh manager's conceptual starting point is `Idle`, but that must
    // not make the *first* explicit `report_idle()` call (as `activate()`
    // makes right after constructing a Herdr-backed manager) a silent no-op
    // against that same starting value - Herdr would never be told the
    // source exists at all until some later, unrelated state change
    // happened to occur first.
    let (manager, reporter) = manager_with_recorder();
    manager.report_idle();
    assert_eq!(
        calls_of(&reporter).len(),
        1,
        "the first report must always go out, even when it repeats the manager's initial state"
    );
}

#[test]
fn null_reporter_reports_inactive_so_a_forked_child_never_reactivates_needlessly() {
    // `reactivate_after_fork` uses this to decide whether a forked child
    // needs a worker thread of its own; it must stay a no-op whenever Herdr
    // was never active in the parent to begin with.
    assert!(!NullReporter.is_active());
    assert!(RecordingReporter::default().is_active());
}

#[test]
fn begin_yield_releases_and_the_guards_drop_reclaims_the_prior_state() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    assert!(!manager.is_yielded());

    let guard = manager.begin_yield();
    assert!(manager.is_yielded());
    assert!(
        matches!(calls_of(&reporter).last(), Some(Call::Release(_))),
        "begin_yield must release this pane's authority"
    );

    drop(guard);
    assert!(!manager.is_yielded());
    let calls = calls_of(&reporter);
    assert!(
        matches!(calls.last(), Some(Call::Report(AgentState::Working, _))),
        "end_yield must reclaim with the state from before the yield: {calls:?}"
    );
}

#[test]
fn reports_made_while_yielded_are_tracked_but_never_sent() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_idle();
    let guard = manager.begin_yield();
    let calls_before = calls_of(&reporter).len();

    manager.report_working();
    manager.report_working(); // dedup still applies while yielded
    assert_eq!(
        calls_of(&reporter).len(),
        calls_before,
        "a state change while authority is on loan must not reach the reporter"
    );

    drop(guard);
    let calls = calls_of(&reporter);
    assert!(
        matches!(calls.last(), Some(Call::Report(AgentState::Working, _))),
        "end_yield must reclaim with the latest state tracked while yielded, \
         not the pre-yield one: {calls:?}"
    );
}

#[test]
fn reclaim_after_a_blocked_yield_restores_blocked_not_idle() {
    let (manager, reporter) = manager_with_recorder();
    let guard = manager.begin_yield();
    manager.report_blocked("needs approval");
    drop(guard);
    let calls = calls_of(&reporter);
    assert!(
        matches!(calls.last(), Some(Call::Report(AgentState::Blocked(_), _))),
        "reclaim must resend Blocked, not fall back to Idle: {calls:?}"
    );
}

#[test]
fn nested_yields_release_and_reclaim_exactly_once() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();

    let outer = manager.begin_yield();
    let inner = manager.begin_yield();
    let release_count = |reporter: &RecordingReporter| {
        calls_of(reporter)
            .iter()
            .filter(|c| matches!(c, Call::Release(_)))
            .count()
    };
    assert_eq!(
        release_count(&reporter),
        1,
        "only the outermost begin_yield should release"
    );

    drop(inner);
    assert!(
        manager.is_yielded(),
        "dropping the inner guard must not end the yield"
    );

    drop(outer);
    assert!(!manager.is_yielded());
    assert_eq!(release_count(&reporter), 1, "still only one release total");
}

#[test]
fn reconcile_is_suppressed_while_yielded() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    let guard = manager.begin_yield();

    let calls_before = calls_of(&reporter).len();
    manager.reconcile_if_stale();
    assert_eq!(
        calls_of(&reporter).len(),
        calls_before,
        "reconcile_if_stale must not resend while authority is on loan"
    );

    drop(guard);
}

#[test]
fn seq_stays_strictly_increasing_across_a_yield_and_reclaim() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    let guard = manager.begin_yield();
    manager.report_blocked("waiting");
    drop(guard);
    manager.report_idle();

    let calls = calls_of(&reporter);
    let seqs: Vec<u64> = calls
        .iter()
        .map(|c| match c {
            Call::Report(_, seq) => *seq,
            Call::Release(seq) => *seq,
        })
        .collect();
    for pair in seqs.windows(2) {
        assert!(
            pair[1] > pair[0],
            "seq must strictly increase across yield/reclaim: {seqs:?}"
        );
    }
}

#[test]
fn a_yield_guard_dropped_after_shutdown_never_reclaims() {
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    let guard = manager.begin_yield();
    manager.shutdown();
    let calls_before = calls_of(&reporter).len();

    drop(guard);
    assert_eq!(
        calls_of(&reporter).len(),
        calls_before,
        "a guard dropped after shutdown must not re-claim authority this process already gave up"
    );
}

#[test]
fn begin_yield_after_shutdown_never_releases() {
    // Mirrors `emit`'s own post-shutdown guard: a `begin_yield` call that
    // races an emergency signal-triggered `shutdown()` (a separate task,
    // per `dsh/src/lib.rs`'s SIGTERM/SIGHUP watcher) must not send a stray
    // `release-agent` for a process that already gave up authority.
    let (manager, reporter) = manager_with_recorder();
    manager.report_working();
    manager.shutdown();
    let calls_before = calls_of(&reporter).len();

    let guard = manager.begin_yield();
    assert!(
        !manager.is_yielded(),
        "begin_yield after shutdown must not even bump yield_depth"
    );
    assert_eq!(
        calls_of(&reporter).len(),
        calls_before,
        "begin_yield after shutdown must not release"
    );

    drop(guard);
    assert_eq!(
        calls_of(&reporter).len(),
        calls_before,
        "dropping that guard must not reclaim anything either"
    );
}

#[test]
fn yield_on_a_null_reporter_manager_is_a_pure_no_op() {
    let manager = AgentLifecycleManager::null();
    let guard = manager.begin_yield();
    assert!(!manager.is_yielded(), "NullReporter is never active");
    drop(guard);
    // Nothing to assert beyond "did not panic" - same spirit as
    // `null_reporter_is_a_pure_no_op`.
}
