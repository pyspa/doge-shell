//! Chat state that outlives a single `!` invocation.
//!
//! Every `!` used to start from zero: no follow-up questions, and the agent
//! re-explored the repository from scratch each time, paying for the same tool
//! calls again. The state lives here rather than in the shell because it is
//! chat-runtime state, not shell configuration - the same reason
//! `EnvironmentSnapshot` must not carry it.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::ConversationManager;

/// Environment key overriding how long a conversation is carried forward.
/// `0` disables carrying it forward at all.
pub(super) const SESSION_TTL_KEY: &str = "AI_CHAT_SESSION_TTL_SECS";
const DEFAULT_SESSION_TTL_SECS: u64 = 1800;

struct StoredSession {
    manager: ConversationManager,
    /// Names this conversation for the whole time it is carried forward, so a
    /// hook can tell a follow-up question from a fresh start.
    id: String,
    /// The part of the system prompt that decides continuity: a changed
    /// operator prompt or language means the old conversation no longer
    /// matches. Deliberately *not* the rendered prompt - that also carries the
    /// skills list, and keying on it discarded the conversation at the exact
    /// moment the agent had written a skill.
    identity: String,
    /// The project boundary the conversation was started in
    /// (`tool::workspace_root` of the cwd at the time), not the raw cwd.
    /// `cd src` still resolves to the same project root, so it no longer ends
    /// the conversation - the same boundary the tool sandbox and skill roots
    /// already use.
    scope: Option<PathBuf>,
    stored_at: Instant,
}

static SESSION: LazyLock<Mutex<Option<StoredSession>>> = LazyLock::new(|| Mutex::new(None));

/// The slot, recovering from a panic that happened while it was held.
///
/// `StoredSession` is only ever replaced wholesale, never mutated in place
/// (see `store`), so a poisoned lock cannot be holding a half-written
/// conversation - the panic happened somewhere else while this thread merely
/// held the guard. Propagating the poison instead made every later `!` start
/// from zero and `chat_reset` answer "no chat session to clear" while one was
/// still there.
fn slot() -> MutexGuard<'static, Option<StoredSession>> {
    SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve the carry-forward window from an already-read setting.
///
/// The caller reads it shell-variable first: `proxy.set_var` writes into the
/// shell `Environment`, so an env-only lookup here would ignore `(vset ...)`.
/// `0` disables carrying the conversation forward.
pub(super) fn resolve_ttl(setting: Option<String>) -> Option<Duration> {
    let secs = setting
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SESSION_TTL_SECS);

    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Why the stored conversation does not apply to this turn, or `None` when it
/// still does. Shared by `take` (which needs the reason, to tell the user why
/// it started over) and `peek_id` (which only needs the yes/no).
///
/// Collects every reason that applies, not just the first: age and identity
/// and scope can all disagree at once, and reporting only one used to leave
/// the user thinking a single change explained a restart that two changes
/// together caused.
fn mismatch(
    stored: &StoredSession,
    ttl: Duration,
    identity: &str,
    scope: Option<&Path>,
) -> Option<String> {
    let reasons = mismatch_reasons(stored, ttl, identity, scope);
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// The individual reasons, for callers like `chat_status` that report more
/// than a single joined line.
fn mismatch_reasons(
    stored: &StoredSession,
    ttl: Duration,
    identity: &str,
    scope: Option<&Path>,
) -> Vec<String> {
    let mut reasons = Vec::new();

    let age = stored.stored_at.elapsed();
    if age > ttl {
        reasons.push(format!("the previous one went idle for {}s", age.as_secs()));
    }
    if stored.identity != identity {
        // `identity` also carries the MCP system-prompt fragment (see
        // `StoredSession::identity`'s doc comment), so a connect/disconnect
        // lands here too - named honestly, since which of the three actually
        // changed cannot be told apart from a string comparison alone.
        reasons.push("the prompt, language, or MCP connections changed".to_string());
    }
    if stored.scope.as_deref() != scope {
        reasons.push(match &stored.scope {
            Some(previous) => format!("the project changed from {}", previous.display()),
            None => "the project changed".to_string(),
        });
    }

    reasons
}

/// What a non-destructive continuity check finds. Unlike [`Claim`], this
/// never consumes the stored conversation, so `chat_status` can answer
/// "would a follow-up `!` continue this?" without discarding it.
pub enum Continuity {
    Continued {
        id: String,
        messages: usize,
        age: Duration,
        scope: Option<PathBuf>,
    },
    Fresh {
        /// Whether anything is stored at all. False is the ordinary "no
        /// conversation yet" (or TTL disabled); true with reasons means a
        /// stored conversation the next turn would drop.
        stored: bool,
        reasons: Vec<String>,
    },
}

/// Check continuity without consuming the stored conversation.
pub fn check(ttl: Option<Duration>, identity: &str, scope: Option<&Path>) -> Continuity {
    let Some(ttl) = ttl else {
        return Continuity::Fresh {
            stored: slot().is_some(),
            reasons: Vec::new(),
        };
    };
    let guard = slot();
    let Some(stored) = guard.as_ref() else {
        return Continuity::Fresh {
            stored: false,
            reasons: Vec::new(),
        };
    };
    let reasons = mismatch_reasons(stored, ttl, identity, scope);
    if reasons.is_empty() {
        Continuity::Continued {
            id: stored.id.clone(),
            messages: stored.manager.buffer.len(),
            age: stored.stored_at.elapsed(),
            scope: stored.scope.clone(),
        }
    } else {
        Continuity::Fresh {
            stored: true,
            reasons,
        }
    }
}

/// Idle time under which a continued conversation counts as about to expire.
///
/// Warns while there is still a turn left to act on it, rather than reporting
/// the expiry after the fact.
pub(super) const SESSION_EXPIRY_SOON_SECS: u64 = 60;

/// Whether a continued conversation's remaining idle window is under
/// [`SESSION_EXPIRY_SOON_SECS`]. Pure, so the "expires soon" notice in the
/// turn header is unit-testable without driving a turn.
pub(super) fn expiry_soon(ttl: Duration, stored_at: Instant) -> bool {
    ttl.checked_sub(stored_at.elapsed())
        .is_some_and(|left| left.as_secs() < SESSION_EXPIRY_SOON_SECS)
}

/// A conversation this turn gets to continue.
pub(super) struct Carried {
    pub(super) manager: ConversationManager,
    pub(super) id: String,
    /// When the conversation was last extended by a *successful* turn. Passed
    /// back into `store` so a run of failed turns cannot keep a dead
    /// conversation's idle clock running forever.
    pub(super) stored_at: Instant,
}

/// The result of claiming the stored conversation.
pub(super) enum Claim {
    Continued(Carried),
    /// `Some` names why the previous conversation was dropped, for the one
    /// line a turn prints. `None` is the ordinary first `!` of a shell, where
    /// there is nothing to explain.
    Fresh(Option<String>),
}

/// Claim the stored conversation when it still matches this turn.
///
/// Always removes it first, so a turn that dies mid-flight cannot leave a
/// stale conversation behind.
pub(super) fn take(ttl: Option<Duration>, identity: &str, scope: Option<&Path>) -> Claim {
    let Some(ttl) = ttl else {
        return Claim::Fresh(None);
    };
    let Some(stored) = slot().take() else {
        return Claim::Fresh(None);
    };

    if let Some(reason) = mismatch(&stored, ttl, identity, scope) {
        return Claim::Fresh(Some(reason));
    }

    Claim::Continued(Carried {
        manager: stored.manager,
        id: stored.id,
        stored_at: stored.stored_at,
    })
}

/// The id this turn will continue, without consuming the conversation.
///
/// Hooks that fire before `take` still have to name the conversation, and
/// `take` is destructive by design: calling it early to learn the id would
/// throw the conversation away whenever the turn was then refused.
pub(super) fn peek_id(
    ttl: Option<Duration>,
    identity: &str,
    scope: Option<&Path>,
) -> Option<String> {
    let ttl = ttl?;
    let slot = slot();
    let stored = slot.as_ref()?;

    mismatch(stored, ttl, identity, scope)
        .is_none()
        .then(|| stored.id.clone())
}

// `stored_at`: `None` for a turn that finished - the idle clock restarts from
// now. `Some(previous)` for a failed turn that was rewound to what a
// completed turn had stored - the conversation did not move forward, so its
// clock must not either, or a retry loop could keep it alive indefinitely.
pub(super) fn store(
    ttl: Option<Duration>,
    manager: ConversationManager,
    id: &str,
    identity: &str,
    scope: Option<PathBuf>,
    stored_at: Option<Instant>,
) {
    if ttl.is_none() {
        return;
    }

    *slot() = Some(StoredSession {
        manager,
        id: id.to_string(),
        identity: identity.to_string(),
        scope,
        stored_at: stored_at.unwrap_or_else(Instant::now),
    });
}

/// Describe the carried conversation for `chat_reset`, `chat_status` and
/// `doctor`. `ttl` is the setting this turn resolved, so the remaining idle
/// window can be shown - `session.rs` itself has no `proxy` to resolve it.
///
/// Returns `None` whenever the next `!` would not continue this conversation
/// on age/ttl grounds alone - either because `ttl` is disabled, or because
/// the stored conversation has gone idle past it - even though something is
/// still sitting in the slot. Describing a conversation as "carried" when the
/// very next `!` would in fact start fresh is worse than describing nothing:
/// `chat_status`'s whole point is to answer "would a follow-up `!` continue
/// this?", and an idle-only answer is the most this module can give without a
/// `proxy` to rebuild `identity`/`scope` from (a full recomputation needs the
/// MCP manager, which only `ChatToolHost` - not the base `ShellProxy` every
/// builtin is called with - can reach). An operator prompt, language or
/// project change since the conversation was stored is not reflected here.
pub fn session_description(ttl: Option<Duration>) -> Option<String> {
    let slot = slot();
    let stored = slot.as_ref()?;
    let age = stored.stored_at.elapsed();
    if ttl.is_none_or(|ttl| age > ttl) {
        return None;
    }
    Some(format!(
        "{} - {} message(s), {}s old{}{}",
        stored.id,
        stored.manager.buffer.len(),
        age.as_secs(),
        stored
            .scope
            .as_ref()
            .map(|scope| format!(", root {}", scope.display()))
            .unwrap_or_default(),
        ttl.and_then(|ttl| ttl.checked_sub(age))
            .map(|left| format!(", idle for {}s more", left.as_secs()))
            .unwrap_or_default(),
    ))
}

/// Drop the carried conversation. Returns true when there was one.
pub fn session_reset() -> bool {
    slot().take().is_some()
}

#[cfg(test)]
pub(super) mod tests {
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

    #[test]
    fn a_stored_conversation_is_reused_for_the_same_prompt_and_scope() {
        let _guard = TEST_LOCK.lock().unwrap();
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
        session_reset();

        store(ttl(), manager(), "abc123", "sys", None, None);
        let description = session_description(ttl()).expect("carried");
        assert!(description.contains("abc123"));
        assert!(description.contains("idle for"));
    }

    #[test]
    fn a_stale_session_is_not_described_as_carried() {
        let _guard = TEST_LOCK.lock().unwrap();
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
        session_reset();

        store(ttl(), manager(), "s1", "sys", None, None);

        assert!(session_description(None).is_none());
    }

    #[test]
    fn a_poisoned_lock_does_not_hide_the_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
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
}
