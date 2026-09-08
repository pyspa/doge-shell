//! Chat state that outlives a single `!` invocation.
//!
//! Every `!` used to start from zero: no follow-up questions, and the agent
//! re-explored the repository from scratch each time, paying for the same tool
//! calls again. The state lives here rather than in the shell because it is
//! chat-runtime state, not shell configuration - the same reason
//! `EnvironmentSnapshot` must not carry it.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
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
    /// The part of the system prompt that decides continuity: a changed model,
    /// language or operator prompt means the old conversation no longer
    /// matches. Deliberately *not* the rendered prompt - that also carries the
    /// skills list, and keying on it discarded the conversation at the exact
    /// moment the agent had written a skill.
    identity: String,
    cwd: Option<PathBuf>,
    stored_at: Instant,
}

static SESSION: LazyLock<Mutex<Option<StoredSession>>> = LazyLock::new(|| Mutex::new(None));

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

/// Claim the stored conversation when it still matches this turn.
///
/// Always removes it, so a turn that dies mid-flight cannot leave a stale
/// conversation behind.
fn still_applies(
    stored: &StoredSession,
    ttl: Duration,
    identity: &str,
    cwd: Option<&Path>,
) -> bool {
    stored.stored_at.elapsed() <= ttl && stored.identity == identity && stored.cwd.as_deref() == cwd
}

pub(super) fn take(
    ttl: Option<Duration>,
    identity: &str,
    cwd: Option<&Path>,
) -> Option<(ConversationManager, String)> {
    let ttl = ttl?;
    let stored = SESSION.lock().ok()?.take()?;

    if !still_applies(&stored, ttl, identity, cwd) {
        return None;
    }

    Some((stored.manager, stored.id))
}

/// The id this turn will continue, without consuming the conversation.
///
/// Hooks that fire before `take` still have to name the conversation, and
/// `take` is destructive by design: calling it early to learn the id would
/// throw the conversation away whenever the turn was then refused.
pub(super) fn peek_id(ttl: Option<Duration>, identity: &str, cwd: Option<&Path>) -> Option<String> {
    let ttl = ttl?;
    let slot = SESSION.lock().ok()?;
    let stored = slot.as_ref()?;

    still_applies(stored, ttl, identity, cwd).then(|| stored.id.clone())
}

pub(super) fn store(
    ttl: Option<Duration>,
    manager: ConversationManager,
    id: &str,
    identity: &str,
    cwd: Option<PathBuf>,
) {
    if ttl.is_none() {
        return;
    }

    if let Ok(mut slot) = SESSION.lock() {
        *slot = Some(StoredSession {
            manager,
            id: id.to_string(),
            identity: identity.to_string(),
            cwd,
            stored_at: Instant::now(),
        });
    }
}

/// Describe the carried conversation for `chat_reset` and `doctor`.
pub fn session_description() -> Option<String> {
    let slot = SESSION.lock().ok()?;
    let stored = slot.as_ref()?;
    Some(format!(
        "{} message(s), {}s old{}",
        stored.manager.buffer.len(),
        stored.stored_at.elapsed().as_secs(),
        stored
            .cwd
            .as_ref()
            .map(|cwd| format!(", cwd {}", cwd.display()))
            .unwrap_or_default()
    ))
}

/// Drop the carried conversation. Returns true when there was one.
pub fn session_reset() -> bool {
    match SESSION.lock() {
        Ok(mut slot) => slot.take().is_some(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
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

    /// The store is process-wide, so these run under one lock.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_stored_conversation_is_reused_for_the_same_prompt_and_cwd() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        let cwd = PathBuf::from("/tmp/project");
        store(ttl(), manager(), "s1", "sys", Some(cwd.clone()));

        assert!(take(ttl(), "sys", Some(&cwd)).is_some());
        // take() consumes it.
        assert!(take(ttl(), "sys", Some(&cwd)).is_none());
    }

    #[test]
    fn a_changed_system_prompt_starts_a_new_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        store(ttl(), manager(), "s1", "sys", None);
        assert!(take(ttl(), "different", None).is_none());
    }

    #[test]
    fn a_changed_directory_starts_a_new_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        store(ttl(), manager(), "s1", "sys", Some(PathBuf::from("/a")));
        assert!(take(ttl(), "sys", Some(Path::new("/b"))).is_none());
    }

    #[test]
    fn a_zero_ttl_disables_carrying_the_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        let disabled = resolve_ttl(Some("0".to_string()));
        assert!(disabled.is_none());

        store(disabled, manager(), "s1", "sys", None);
        assert!(session_description().is_none());
        assert!(take(disabled, "sys", None).is_none());
    }

    /// A follow-up question stays the same conversation, so the id a hook sees
    /// has to survive the round trip.
    #[test]
    fn the_session_id_is_carried_with_the_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        store(ttl(), manager(), "abc123", "sys", None);
        let (_manager, id) = take(ttl(), "sys", None).expect("carried");
        assert_eq!(id, "abc123");
    }

    #[test]
    fn peek_does_not_consume_the_session() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        store(ttl(), manager(), "abc123", "sys", None);

        assert_eq!(peek_id(ttl(), "sys", None).as_deref(), Some("abc123"));
        assert_eq!(peek_id(ttl(), "sys", None).as_deref(), Some("abc123"));
        assert!(peek_id(ttl(), "other", None).is_none());
        // Still there for the turn that actually claims it.
        assert!(take(ttl(), "sys", None).is_some());
    }

    #[test]
    fn reset_reports_whether_it_cleared_anything() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        assert!(!session_reset());
        store(ttl(), manager(), "s1", "sys", None);
        assert!(session_description().is_some());
        assert!(session_reset());
        assert!(session_description().is_none());
    }
}
