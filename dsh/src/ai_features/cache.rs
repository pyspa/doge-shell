//! Short-lived cache for read-only AI analyses.
//!
//! Explaining the same command twice, or diagnosing the same failure again
//! after scrolling back, used to bill twice for an identical answer. Only
//! side-effect-free analyses are cached; anything that can run a tool is not.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long an answer stays usable.
///
/// Deliberately short: process-environment changes outside the shell's variable
/// setters are not observable here. Shell-side `AI_MESSAGE_LANG` changes clear
/// the cache explicitly before another answer can be reused.
const TTL: Duration = Duration::from_secs(60);
/// Upper bound on retained answers, evicted oldest-first.
const MAX_ENTRIES: usize = 64;

struct Entry {
    answer: String,
    stored_at: Instant,
}

static CACHE: LazyLock<Mutex<HashMap<u64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Settings that change what a correct answer looks like.
fn answer_scope() -> (String, String) {
    let model = std::env::var("AI_CHAT_MODEL")
        .or_else(|_| std::env::var("OPENAI_MODEL"))
        .unwrap_or_default();
    let language = std::env::var("AI_MESSAGE_LANG").unwrap_or_default();
    (model, language)
}

fn key(kind: &str, inputs: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    let (model, language) = answer_scope();
    model.hash(&mut hasher);
    language.hash(&mut hasher);
    for input in inputs {
        input.hash(&mut hasher);
    }
    hasher.finish()
}

/// Answer for a previous identical request, if it is still fresh.
pub fn lookup(kind: &str, inputs: &[&str]) -> Option<String> {
    let key = key(kind, inputs);
    let mut cache = CACHE.lock().ok()?;

    match cache.get(&key) {
        Some(entry) if entry.stored_at.elapsed() <= TTL => Some(entry.answer.clone()),
        Some(_) => {
            cache.remove(&key);
            None
        }
        None => None,
    }
}

pub fn store(kind: &str, inputs: &[&str], answer: &str) {
    if answer.trim().is_empty() {
        return;
    }

    let Ok(mut cache) = CACHE.lock() else {
        return;
    };

    if cache.len() >= MAX_ENTRIES {
        evict_oldest(&mut cache);
    }

    cache.insert(
        key(kind, inputs),
        Entry {
            answer: answer.to_string(),
            stored_at: Instant::now(),
        },
    );
}

fn evict_oldest(cache: &mut HashMap<u64, Entry>) {
    let oldest = cache
        .iter()
        .min_by_key(|(_, entry)| entry.stored_at)
        .map(|(key, _)| *key);
    if let Some(key) = oldest {
        cache.remove(&key);
    }
}

pub(crate) fn clear() {
    if let Ok(mut cache) = CACHE.lock() {
        cache.clear();
    }
}

/// Serializes every test that touches the process-wide cache.
///
/// The cache tests below and the feature tests in `tests.rs` share one global
/// store, and the feature tests call `clear()`. While only one of the two
/// groups took a lock, a `clear()` from the other could land between a
/// `store` and its `lookup`, so the suite failed a few runs in ten.
///
/// A `tokio` mutex, because the feature tests are async and hold this across
/// an `.await`; it also has no poisoning, so one failing test cannot cascade.
#[cfg(test)]
pub(super) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Take that lock from a synchronous test.
#[cfg(test)]
pub(super) fn blocking_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.blocking_lock()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_returned_for_an_identical_request() {
        let _guard = blocking_test_guard();
        clear();

        assert!(lookup("explain", &["git status"]).is_none());
        store("explain", &["git status"], "shows the working tree");
        assert_eq!(
            lookup("explain", &["git status"]).as_deref(),
            Some("shows the working tree")
        );
    }

    #[test]
    fn a_different_input_or_kind_misses() {
        let _guard = blocking_test_guard();
        clear();

        store("explain", &["git status"], "answer");
        assert!(lookup("explain", &["git log"]).is_none());
        assert!(lookup("diagnose", &["git status"]).is_none());
    }

    #[test]
    fn empty_answers_are_not_stored() {
        let _guard = blocking_test_guard();
        clear();

        store("explain", &["x"], "   ");
        assert!(lookup("explain", &["x"]).is_none());
    }

    #[test]
    fn a_changed_model_or_language_misses() {
        let _guard = blocking_test_guard();
        clear();

        store("explain", &["ls"], "english answer");
        assert!(lookup("explain", &["ls"]).is_some());

        // SAFETY: single-threaded under TEST_LOCK.
        unsafe { std::env::set_var("AI_MESSAGE_LANG", "Japanese") };
        let missed = lookup("explain", &["ls"]).is_none();
        unsafe { std::env::remove_var("AI_MESSAGE_LANG") };

        assert!(missed, "a language change must not reuse the old answer");
    }

    #[test]
    fn the_cache_stays_bounded() {
        let _guard = blocking_test_guard();
        clear();

        for index in 0..MAX_ENTRIES * 2 {
            store("explain", &[&index.to_string()], "answer");
        }

        assert!(CACHE.lock().unwrap().len() <= MAX_ENTRIES);
    }
}
