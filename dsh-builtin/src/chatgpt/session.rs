//! Chat state that outlives a single `!` invocation.
//!
//! Every `!` used to start from zero: no follow-up questions, and the agent
//! re-explored the repository from scratch each time, paying for the same tool
//! calls again. The state lives here rather than in the shell because it is
//! chat-runtime state, not shell configuration - the same reason
//! `EnvironmentSnapshot` must not carry it.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Version of the on-disk session file. A file with any other version is
/// ignored, never migrated: a conversation is easy to rebuild, and guessing
/// at an unknown shape risks resuming the wrong one.
const PERSISTED_VERSION: u32 = 1;

/// The on-disk form of [`StoredSession`]. `Instant` cannot cross a restart,
/// so the file carries wall-clock seconds and the reader converts back to a
/// monotonic timestamp - a clock jump only shifts the idle calculation, it
/// can never resurrect half a conversation.
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedSession {
    version: u32,
    manager: ConversationManager,
    id: String,
    identity: String,
    scope: Option<PathBuf>,
    stored_at_unix_secs: u64,
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Rebuild the monotonic timestamp from a wall-clock age. A timestamp in the
/// future reads as age zero (continue), never as expired: a clock skew must
/// not discard a live conversation.
fn instant_from_unix_secs(stored_at_unix_secs: u64) -> Instant {
    let age_secs = now_unix_secs().saturating_sub(stored_at_unix_secs);
    Instant::now()
        .checked_sub(Duration::from_secs(age_secs))
        .unwrap_or_else(Instant::now)
}

fn read_persisted() -> Option<StoredSession> {
    let text = std::fs::read_to_string(crate::config_paths::chat_session_file()).ok()?;
    let persisted: PersistedSession = serde_json::from_str(&text).ok()?;
    if persisted.version != PERSISTED_VERSION {
        return None;
    }
    Some(StoredSession {
        manager: persisted.manager,
        id: persisted.id,
        identity: persisted.identity,
        scope: persisted.scope,
        stored_at: instant_from_unix_secs(persisted.stored_at_unix_secs),
    })
}

/// Best effort: a turn that finished must never fail because the disk did.
/// Callers ignore the return value.
fn write_persisted(persisted: &PersistedSession) -> bool {
    let path = crate::config_paths::chat_session_file();
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    serde_json::to_string(persisted)
        .map(|text| std::fs::write(&path, text).is_ok())
        .unwrap_or(false)
}

/// True when a session file was removed.
fn remove_persisted() -> bool {
    std::fs::remove_file(crate::config_paths::chat_session_file()).is_ok()
}

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
            stored: slot().is_some()
                || crate::config_paths::chat_session_file().is_file(),
            reasons: Vec::new(),
        };
    };
    if let Some(found) = slot().as_ref().map(|stored| {
        (
            stored.id.clone(),
            stored.manager.buffer.len(),
            stored.stored_at.elapsed(),
            stored.scope.clone(),
            mismatch_reasons(stored, ttl, identity, scope),
        )
    }) {
        let (id, messages, age, scope, reasons) = found;
        if reasons.is_empty() {
            return Continuity::Continued {
                id,
                messages,
                age,
                scope,
            };
        }
        return Continuity::Fresh {
            stored: true,
            reasons,
        };
    }
    // Restart recovery: the slot is empty but a previous shell may have left
    // a conversation on disk.
    if let Some(stored) = read_persisted() {
        let reasons = mismatch_reasons(&stored, ttl, identity, scope);
        if reasons.is_empty() {
            return Continuity::Continued {
                id: stored.id,
                messages: stored.manager.buffer.len(),
                age: stored.stored_at.elapsed(),
                scope: stored.scope,
            };
        }
        return Continuity::Fresh {
            stored: true,
            reasons,
        };
    }
    Continuity::Fresh {
        stored: false,
        reasons: Vec::new(),
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
    // A restart leaves the slot empty with the conversation on disk.
    let Some(stored) = slot().take().or_else(read_persisted) else {
        return Claim::Fresh(None);
    };

    if let Some(reason) = mismatch(&stored, ttl, identity, scope) {
        return Claim::Fresh(Some(reason));
    }

    // The turn owns the conversation now: drop the file copy so a later
    // `take` cannot resurrect it when this turn ends without storing (a new
    // conversation that fails stores nothing). A mismatch above leaves the
    // file alone - returning to the old project can still resume it until
    // the next successful turn overwrites it.
    remove_persisted();

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
    if let Some(id) = slot().as_ref().and_then(|stored| {
        mismatch(stored, ttl, identity, scope)
            .is_none()
            .then(|| stored.id.clone())
    }) {
        return Some(id);
    }
    // Same restart fallback as `take`, without consuming anything.
    read_persisted().and_then(|stored| {
        mismatch(&stored, ttl, identity, scope)
            .is_none()
            .then(|| stored.id)
    })
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

    // A rewound turn re-stores the instant it carried so the idle clock does
    // not restart; the file must name the same wall-clock time.
    let now_unix = now_unix_secs();
    let stored_unix = stored_at
        .map(|at| now_unix.saturating_sub(at.elapsed().as_secs()))
        .unwrap_or(now_unix);
    let persisted = PersistedSession {
        version: PERSISTED_VERSION,
        manager,
        id: id.to_string(),
        identity: identity.to_string(),
        scope,
        stored_at_unix_secs: stored_unix,
    };
    // Serialize before the move below: the manager cannot be cloned back out
    // of the slot afterwards. A disk failure must never fail the turn.
    write_persisted(&persisted);
    *slot() = Some(StoredSession {
        manager: persisted.manager,
        id: persisted.id,
        identity: persisted.identity,
        scope: persisted.scope,
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
fn describe(
    id: &str,
    messages: usize,
    age: Duration,
    scope: Option<&PathBuf>,
    ttl: Option<Duration>,
) -> String {
    format!(
        "{} - {} message(s), {}s old{}{}",
        id,
        messages,
        age.as_secs(),
        scope
            .map(|scope| format!(", root {}", scope.display()))
            .unwrap_or_default(),
        ttl.and_then(|ttl| ttl.checked_sub(age))
            .map(|left| format!(", idle for {}s more", left.as_secs()))
            .unwrap_or_default(),
    )
}

pub fn session_description(ttl: Option<Duration>) -> Option<String> {
    if ttl.is_none() {
        return None;
    }
    if let Some(found) = slot().as_ref().map(|stored| {
        (
            stored.id.clone(),
            stored.manager.buffer.len(),
            stored.stored_at.elapsed(),
            stored.scope.clone(),
        )
    }) {
        let (id, messages, age, scope) = found;
        if ttl.is_none_or(|ttl| age > ttl) {
            return None;
        }
        return Some(describe(&id, messages, age, scope.as_ref(), ttl));
    }
    // Restart recovery without identity/scope recomputation, like `check`:
    // only the age is knowable here, for the same documented reason.
    let persisted = read_persisted()?;
    let age = persisted.stored_at.elapsed();
    if ttl.is_none_or(|ttl| age > ttl) {
        return None;
    }
    Some(describe(
        &persisted.id,
        persisted.manager.buffer.len(),
        age,
        persisted.scope.as_ref(),
        ttl,
    ))
}

/// Drop the carried conversation, in memory and on disk. Returns true when
/// there was one.
pub fn session_reset() -> bool {
    let had_slot = slot().take().is_some();
    let had_file = remove_persisted();
    had_slot || had_file
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
}
