use super::*;
use serde_json::json;

fn ttl() -> Option<Duration> {
    resolve_ttl(None)
}

fn manager() -> ConversationManager {
    ConversationManager::new(
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "goal"}),
    )
}

/// The store is process-wide, so these run under one lock. Shared with
/// other test modules in this crate that seed the slot.
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Redirect the state home (where the session file lives) for one test.
///
/// `store` now writes a file beside the slot, so without this every test
/// below would read and write the developer's real session file - and a
/// `session_reset` would delete a real conversation. The crate-wide env
/// lock is shared with every other `XDG_STATE_HOME` user; taken after
/// `TEST_LOCK`, an order nothing else reverses. `pub(crate)` because the
/// `commands` tests seed the same process-wide slot.
pub(crate) struct StateHomeGuard {
    _env: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl Drop for StateHomeGuard {
    fn drop(&mut self) {
        // SAFETY: single-threaded under the env lock.
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
    }
}

pub(crate) fn isolated_state_home() -> StateHomeGuard {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded under the env lock.
    let env = crate::chatgpt::tool::execute::tests::env_lock();
    let previous = std::env::var_os("XDG_STATE_HOME");
    unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
    StateHomeGuard {
        _env: env,
        _dir: dir,
        previous,
    }
}

#[test]
fn a_stored_conversation_is_reused_for_the_same_prompt_and_scope() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let scope = PathBuf::from("/tmp/project");
    store(ttl(), manager(), "s1", "sys", Some(scope.clone()), None);

    assert!(matches!(
        take(ttl(), "sys", Some(&scope)),
        Claim::Continued(_)
    ));
    // take() consumes it.
    assert!(matches!(
        take(ttl(), "sys", Some(&scope)),
        Claim::Fresh(None)
    ));
}

#[test]
fn a_changed_system_prompt_starts_a_new_conversation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);
    match take(ttl(), "different", None) {
        Claim::Fresh(Some(reason)) => assert!(reason.contains("prompt")),
        other => panic!("expected a reasoned Fresh claim, got {}", describe(&other)),
    }
}

#[test]
fn a_changed_project_starts_a_new_conversation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(
        ttl(),
        manager(),
        "s1",
        "sys",
        Some(PathBuf::from("/a")),
        None,
    );
    match take(ttl(), "sys", Some(Path::new("/b"))) {
        Claim::Fresh(Some(reason)) => assert!(reason.contains("project changed from /a")),
        other => panic!("expected a reasoned Fresh claim, got {}", describe(&other)),
    }
}

#[test]
fn multiple_simultaneous_mismatches_are_all_named() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(
        ttl(),
        manager(),
        "s1",
        "sys",
        Some(PathBuf::from("/a")),
        None,
    );
    match take(ttl(), "different", Some(Path::new("/b"))) {
        Claim::Fresh(Some(reason)) => {
            assert!(reason.contains("prompt"), "{reason}");
            assert!(reason.contains("project changed from /a"), "{reason}");
        }
        other => panic!("expected a reasoned Fresh claim, got {}", describe(&other)),
    }
}

/// `session.rs` only ever compares the scope it was given; normalizing a
/// directory to its project root (`tool::workspace_root`) happens in the
/// caller. As long as the caller resolves two directories under the same
/// project to the same scope, this layer keeps the conversation.
#[test]
fn a_directory_below_the_project_root_keeps_the_conversation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let project_root = PathBuf::from("/repo");
    store(
        ttl(),
        manager(),
        "s1",
        "sys",
        Some(project_root.clone()),
        None,
    );
    // `cd src` resolves to the same project root, so the caller passes the
    // same scope, and the conversation continues.
    assert!(matches!(
        take(ttl(), "sys", Some(&project_root)),
        Claim::Continued(_)
    ));
}

#[test]
fn a_zero_ttl_disables_carrying_the_conversation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let disabled = resolve_ttl(Some("0".to_string()));
    assert!(disabled.is_none());

    store(disabled, manager(), "s1", "sys", None, None);
    assert!(session_description(disabled).is_none());
    assert!(matches!(take(disabled, "sys", None), Claim::Fresh(None)));
}

/// A follow-up question stays the same conversation, so the id a hook sees
/// has to survive the round trip.
#[test]
fn the_session_id_is_carried_with_the_conversation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "abc123", "sys", None, None);
    match take(ttl(), "sys", None) {
        Claim::Continued(carried) => assert_eq!(carried.id, "abc123"),
        Claim::Fresh(reason) => {
            panic!("expected the conversation to carry, reason: {reason:?}")
        }
    }
}

#[test]
fn peek_does_not_consume_the_session() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "abc123", "sys", None, None);

    assert_eq!(peek_id(ttl(), "sys", None).as_deref(), Some("abc123"));
    assert_eq!(peek_id(ttl(), "sys", None).as_deref(), Some("abc123"));
    assert!(peek_id(ttl(), "other", None).is_none());
    // Still there for the turn that actually claims it.
    assert!(matches!(take(ttl(), "sys", None), Claim::Continued(_)));
}

#[test]
fn reset_reports_whether_it_cleared_anything() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    assert!(!session_reset());
    store(ttl(), manager(), "s1", "sys", None, None);
    assert!(session_description(ttl()).is_some());
    assert!(session_reset());
    assert!(session_description(ttl()).is_none());
}

/// A margin of tens of seconds, not single digits: `Instant::now()`'s
/// epoch is arbitrary (typically boot time), and a single-digit margin
/// could in principle underflow on a runner whose clock reference point
/// is unusually close to "now" (mirrors the 301s margin used the same way
/// in `chatgpt/mcp/mod.rs`'s tests).
fn seconds_ago(secs: u64) -> Instant {
    Instant::now() - Duration::from_secs(secs)
}

#[test]
fn an_idle_conversation_past_the_ttl_is_not_carried_forward() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    // A conversation stored "in the past" by handing `store` an already-
    // elapsed `stored_at` (as a rewound turn does) is what past-ttl looks
    // like from this module's point of view: `stored_at.elapsed()` grows
    // from whatever instant was recorded, so recording one far enough back
    // simulates having gone idle without a real sleep.
    let ttl = Some(Duration::from_secs(5));
    store(ttl, manager(), "s1", "sys", None, Some(seconds_ago(20)));

    match take(ttl, "sys", None) {
        Claim::Fresh(Some(reason)) => assert!(reason.contains("idle")),
        other => panic!("expected an idle Fresh claim, got {}", describe(&other)),
    }
}

#[test]
fn a_failed_turn_keeps_the_conversation_it_started_from() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);
    let carried = match take(ttl(), "sys", None) {
        Claim::Continued(carried) => carried,
        Claim::Fresh(reason) => {
            panic!("expected the conversation to carry, reason: {reason:?}")
        }
    };
    assert_eq!(carried.manager.buffer.len(), 0);

    // The turn failed and rewound `carried.manager` back to what it was
    // handed (simulated here by reusing the same manager unchanged), then
    // stored it with the carried `stored_at` to keep the idle clock from
    // restarting.
    store(
        ttl(),
        carried.manager,
        "s1",
        "sys",
        None,
        Some(carried.stored_at),
    );

    match take(ttl(), "sys", None) {
        Claim::Continued(carried) => assert_eq!(carried.id, "s1"),
        Claim::Fresh(reason) => {
            panic!("expected the conversation to survive, reason: {reason:?}")
        }
    }
}

#[test]
fn a_rewound_store_does_not_restart_the_idle_clock() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let ttl = Some(Duration::from_secs(30));
    store(ttl, manager(), "s1", "sys", None, Some(seconds_ago(20)));

    let carried = match take(ttl, "sys", None) {
        Claim::Continued(carried) => carried,
        Claim::Fresh(reason) => {
            panic!("expected the conversation to carry, reason: {reason:?}")
        }
    };
    // Rewind: store again with the *carried* `stored_at`, not `Instant::now()`.
    store(
        ttl,
        carried.manager,
        "s1",
        "sys",
        None,
        Some(carried.stored_at),
    );

    // Only ~10s of the 30s ttl should remain - if the clock had restarted,
    // this would still be `Claim::Continued` after the ttl truly elapses,
    // but here we only check that the age reported is close to the
    // original, not reset to ~0.
    let description = session_description(ttl).expect("still carried");
    assert!(
        description.contains("20s old") || description.contains("21s old"),
        "expected the age to reflect the original store, got: {description}"
    );
}

#[test]
fn the_description_names_the_conversation_and_its_remaining_idle_time() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "abc123", "sys", None, None);
    let description = session_description(ttl()).expect("carried");
    assert!(description.contains("abc123"));
    assert!(description.contains("idle for"));
}

#[test]
fn a_stale_session_is_not_described_as_carried() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let ttl = Some(Duration::from_secs(5));
    store(ttl, manager(), "s1", "sys", None, Some(seconds_ago(20)));

    // Something is technically still sitting in the slot, but the next
    // `!` would not continue it - `chat_status` describing it as
    // "carried" would directly contradict what that `!` then does.
    assert!(session_description(ttl).is_none());
}

/// `chat_status` after `AI_CHAT_SESSION_TTL_SECS` is set to `0` used to
/// keep describing whatever was stored while it was still nonzero,
/// making the "carrying is disabled" message it prints for that case
/// unreachable as long as a conversation happened to be present.
#[test]
fn a_disabled_ttl_hides_a_session_that_was_stored_before_it_was_disabled() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);

    assert!(session_description(None).is_none());
}

#[test]
fn a_poisoned_lock_does_not_hide_the_conversation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);

    let result = std::panic::catch_unwind(|| {
        let _guard = SESSION.lock().unwrap();
        panic!("simulated panic while holding the lock");
    });
    assert!(result.is_err());

    assert!(session_description(ttl()).is_some());
    assert_eq!(peek_id(ttl(), "sys", None).as_deref(), Some("s1"));
    assert!(session_reset());
}

fn describe(claim: &Claim) -> &'static str {
    match claim {
        Claim::Continued(_) => "Continued",
        Claim::Fresh(_) => "Fresh",
    }
}

#[test]
fn check_reports_a_matching_conversation_without_consuming_it() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let scope = PathBuf::from("/tmp/project");
    store(ttl(), manager(), "s1", "sys", Some(scope.clone()), None);

    match check(ttl(), "sys", Some(&scope)) {
        Continuity::Continued { id, messages, .. } => {
            assert_eq!(id, "s1");
            assert_eq!(messages, manager().buffer.len());
        }
        Continuity::Fresh { stored, reasons } => {
            panic!("expected Continued, stored={stored} reasons={reasons:?}")
        }
    }
    // Non-destructive: the turn that follows still claims it.
    assert!(matches!(
        take(ttl(), "sys", Some(&scope)),
        Claim::Continued(_)
    ));
}

#[test]
fn check_names_every_reason_a_stored_conversation_would_be_dropped() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(
        ttl(),
        manager(),
        "s1",
        "sys",
        Some(PathBuf::from("/a")),
        None,
    );

    match check(ttl(), "different", Some(Path::new("/b"))) {
        Continuity::Fresh { stored, reasons } => {
            assert!(stored);
            assert!(reasons.iter().any(|r| r.contains("prompt")), "{reasons:?}");
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains("project changed from /a")),
                "{reasons:?}"
            );
        }
        Continuity::Continued { id, .. } => panic!("expected Fresh, got {id}"),
    }
}

#[test]
fn check_reports_an_empty_slot_and_a_disabled_ttl_as_fresh() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    match check(ttl(), "sys", None) {
        Continuity::Fresh { stored, reasons } => {
            assert!(!stored);
            assert!(reasons.is_empty());
        }
        Continuity::Continued { id, .. } => panic!("expected Fresh, got {id}"),
    }

    store(ttl(), manager(), "s1", "sys", None, None);
    match check(None, "sys", None) {
        Continuity::Fresh { stored, reasons } => {
            assert!(stored);
            assert!(reasons.is_empty());
        }
        Continuity::Continued { id, .. } => panic!("expected Fresh, got {id}"),
    }
}

#[test]
fn expiry_soon_fires_only_inside_the_final_minute() {
    // Far from the deadline: no warning.
    assert!(!expiry_soon(
        Duration::from_secs(1800),
        Instant::now() - Duration::from_secs(100)
    ));
    // Inside the final minute: warn.
    assert!(expiry_soon(
        Duration::from_secs(1800),
        Instant::now() - Duration::from_secs(1790)
    ));
    // Already past the deadline reads as expired, not expiring.
    assert!(!expiry_soon(
        Duration::from_secs(60),
        Instant::now() - Duration::from_secs(120)
    ));
}

/// Simulate a restart by dropping the slot without touching the file:
/// the next turn must continue where the previous shell left off.
#[test]
fn a_stored_conversation_survives_a_restart_via_the_session_file() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let mut stored = manager();
    stored.add_message(json!({"role": "user", "content": "follow-up context"}));
    store(ttl(), stored, "restart-1", "sys", None, None);
    // The restart: memory is gone, the file is not.
    slot().take();
    assert!(slot().is_none());

    match take(ttl(), "sys", None) {
        Claim::Continued(carried) => {
            assert_eq!(carried.id, "restart-1");
            assert_eq!(carried.manager.buffer.len(), 1);
        }
        Claim::Fresh(reason) => {
            panic!("expected the file conversation to carry, reason: {reason:?}")
        }
    }
}

/// Claiming is destructive across both layers: once a turn owns the
/// conversation, a second `take` finds nothing anywhere.
#[test]
fn claiming_a_restored_conversation_drops_the_file_copy() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);
    slot().take();
    assert!(matches!(take(ttl(), "sys", None), Claim::Continued(_)));
    assert!(!crate::config_paths::chat_session_file().is_file());
    assert!(matches!(take(ttl(), "sys", None), Claim::Fresh(None)));
}

/// Non-destructive readers see through a restart too.
#[test]
fn check_peek_and_description_see_a_restored_session() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "abc123", "sys", None, None);
    slot().take();

    assert_eq!(peek_id(ttl(), "sys", None).as_deref(), Some("abc123"));
    assert!(matches!(
        check(ttl(), "sys", None),
        Continuity::Continued { id, .. } if id == "abc123"
    ));
    let description = session_description(ttl()).expect("restored session described");
    assert!(description.contains("abc123"), "{description}");
}

/// An expired file is a fresh start, not a resurrection.
#[test]
fn an_expired_file_is_not_continued() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let ttl = Some(Duration::from_secs(5));
    store(ttl, manager(), "s1", "sys", None, Some(seconds_ago(20)));
    slot().take();

    match take(ttl, "sys", None) {
        Claim::Fresh(Some(reason)) => assert!(reason.contains("idle"), "{reason}"),
        other => panic!("expected an idle Fresh claim, got {}", describe(&other)),
    }
}

/// Garbage on disk is a fresh start, never a crash.
#[test]
fn a_corrupt_session_file_is_ignored() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    let path = crate::config_paths::chat_session_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();

    assert!(matches!(take(ttl(), "sys", None), Claim::Fresh(None)));
    assert!(session_description(ttl()).is_none());
}

/// A version from the future is left alone, not guessed at.
#[test]
fn an_unknown_session_version_is_ignored() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);
    let path = crate::config_paths::chat_session_file();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"version\":1"), "{text}");
    std::fs::write(&path, text.replace("\"version\":1", "\"version\":99")).unwrap();
    slot().take();

    assert!(matches!(take(ttl(), "sys", None), Claim::Fresh(None)));
}

/// Reset clears both layers: nothing survives anywhere.
#[test]
fn reset_clears_the_session_file() {
    let _guard = TEST_LOCK.lock().unwrap();
    let _state = isolated_state_home();
    session_reset();

    store(ttl(), manager(), "s1", "sys", None, None);
    assert!(crate::config_paths::chat_session_file().is_file());
    assert!(session_reset());
    assert!(!crate::config_paths::chat_session_file().is_file());
    assert!(matches!(take(ttl(), "sys", None), Claim::Fresh(None)));
}
