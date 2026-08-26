//! Chat state that outlives a single `!` invocation.
//!
//! Every `!` used to start from zero: no follow-up questions, and the agent
//! re-explored the repository from scratch each time, paying for the same tool
//! calls again. The state lives here rather than in the shell because it is
//! chat-runtime state, not shell configuration - the same reason
//! `EnvironmentSnapshot` must not carry it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use dsh_types::mcp::McpServerConfig;

use super::ConversationManager;
use super::mcp::McpManager;

/// Environment key overriding how long a conversation is carried forward.
/// `0` disables carrying it forward at all.
pub(super) const SESSION_TTL_KEY: &str = "AI_CHAT_SESSION_TTL_SECS";
const DEFAULT_SESSION_TTL_SECS: u64 = 1800;
/// MCP server metadata is data only, so it can be reused between turns.
const MCP_CACHE_TTL: Duration = Duration::from_secs(300);

struct StoredSession {
    manager: ConversationManager,
    /// The rendered system prompt. A changed model, language, operator prompt
    /// or skill set means the old conversation no longer matches.
    system_prompt: String,
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
pub(super) fn take(
    ttl: Option<Duration>,
    system_prompt: &str,
    cwd: Option<&Path>,
) -> Option<ConversationManager> {
    let ttl = ttl?;
    let stored = SESSION.lock().ok()?.take()?;

    if stored.stored_at.elapsed() > ttl {
        return None;
    }
    if stored.system_prompt != system_prompt {
        return None;
    }
    if stored.cwd.as_deref() != cwd {
        return None;
    }

    Some(stored.manager)
}

pub(super) fn store(
    ttl: Option<Duration>,
    manager: ConversationManager,
    system_prompt: &str,
    cwd: Option<PathBuf>,
) {
    if ttl.is_none() {
        return;
    }

    if let Ok(mut slot) = SESSION.lock() {
        *slot = Some(StoredSession {
            manager,
            system_prompt: system_prompt.to_string(),
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

struct CachedMcp {
    manager: Arc<McpManager>,
    servers: Vec<McpServerConfig>,
    loaded_at: Instant,
}

static MCP_CACHE: LazyLock<Mutex<Option<CachedMcp>>> = LazyLock::new(|| Mutex::new(None));

/// Load the MCP manager, reusing the previous one when nothing changed.
///
/// This used to reconnect to every configured server on each `!`, which for a
/// stdio server means spawning the process again.
pub(super) fn load_mcp_manager(servers: Vec<McpServerConfig>) -> Arc<McpManager> {
    if let Ok(cache) = MCP_CACHE.lock()
        && let Some(cached) = cache.as_ref()
        && cached.loaded_at.elapsed() < MCP_CACHE_TTL
        && cached.servers == servers
    {
        return Arc::clone(&cached.manager);
    }

    let manager = Arc::new(McpManager::load_blocking(servers.clone()));

    if let Ok(mut cache) = MCP_CACHE.lock() {
        *cache = Some(CachedMcp {
            manager: Arc::clone(&manager),
            servers,
            loaded_at: Instant::now(),
        });
    }

    manager
}

/// Forget the cached MCP manager so the next turn reconnects.
pub fn invalidate_mcp_cache() {
    if let Ok(mut cache) = MCP_CACHE.lock() {
        *cache = None;
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
        store(ttl(), manager(), "sys", Some(cwd.clone()));

        assert!(take(ttl(), "sys", Some(&cwd)).is_some());
        // take() consumes it.
        assert!(take(ttl(), "sys", Some(&cwd)).is_none());
    }

    #[test]
    fn a_changed_system_prompt_starts_a_new_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        store(ttl(), manager(), "sys", None);
        assert!(take(ttl(), "different", None).is_none());
    }

    #[test]
    fn a_changed_directory_starts_a_new_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        store(ttl(), manager(), "sys", Some(PathBuf::from("/a")));
        assert!(take(ttl(), "sys", Some(Path::new("/b"))).is_none());
    }

    #[test]
    fn a_zero_ttl_disables_carrying_the_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        let disabled = resolve_ttl(Some("0".to_string()));
        assert!(disabled.is_none());

        store(disabled, manager(), "sys", None);
        assert!(session_description().is_none());
        assert!(take(disabled, "sys", None).is_none());
    }

    #[test]
    fn reset_reports_whether_it_cleared_anything() {
        let _guard = TEST_LOCK.lock().unwrap();
        session_reset();

        assert!(!session_reset());
        store(ttl(), manager(), "sys", None);
        assert!(session_description().is_some());
        assert!(session_reset());
        assert!(session_description().is_none());
    }
}
