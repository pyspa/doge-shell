use super::command::CompletionCandidate;
use super::display::Candidate;
use super::integrated::EnhancedCandidate;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

/// Trait describing the surface needed for caching completion candidates.
pub trait CacheableCandidate: Clone + std::fmt::Debug {
    fn completion_text(&self) -> &str;
}

impl CacheableCandidate for EnhancedCandidate {
    fn completion_text(&self) -> &str {
        &self.text
    }
}

impl CacheableCandidate for Candidate {
    fn completion_text(&self) -> &str {
        self.get_display_name()
    }
}

impl CacheableCandidate for CompletionCandidate {
    fn completion_text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone)]
struct CacheEntry<T: CacheableCandidate> {
    candidates: Arc<Vec<T>>,
    expires_at: Instant,
}

impl<T: CacheableCandidate> CacheEntry<T> {
    fn new(candidates: Vec<T>, ttl: Duration) -> Self {
        Self {
            candidates: Arc::new(candidates),
            expires_at: Instant::now() + ttl,
        }
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at <= now
    }

    fn extend(&mut self, ttl: Duration) {
        self.expires_at = Instant::now() + ttl;
    }
}

/// Result of a cache lookup
#[derive(Debug, Clone)]
pub struct CacheLookup<T: CacheableCandidate> {
    pub key: String,
    pub candidates: Vec<T>,
    pub exact: bool,
}

/// Completion candidate cache with TTL support
#[derive(Debug)]
pub struct CompletionCache<T: CacheableCandidate> {
    entries: RwLock<HashMap<String, CacheEntry<T>>>,
    pending: RwLock<std::collections::HashSet<String>>,
    default_ttl: Duration,
}

impl<T: CacheableCandidate> CompletionCache<T> {
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            pending: RwLock::new(std::collections::HashSet::new()),
            default_ttl,
        }
    }

    pub fn set(&self, key: String, candidates: Vec<T>) {
        let mut guard = self.entries.write();
        Self::purge_expired_locked(&mut guard);
        debug!("cache set for '{}'. len: {}", key, candidates.len());
        guard.insert(key, CacheEntry::new(candidates, self.default_ttl));
    }

    pub fn mark_pending(&self, key: String) -> bool {
        self.pending.write().insert(key)
    }

    pub fn clear_pending(&self, key: &str) {
        self.pending.write().remove(key);
    }

    pub fn is_pending(&self, key: &str) -> bool {
        self.pending.read().contains(key)
    }

    pub fn extend_ttl(&self, key: &str) -> bool {
        let mut guard = self.entries.write();
        Self::purge_expired_locked(&mut guard);
        if let Some(entry) = guard.get_mut(key) {
            entry.extend(self.default_ttl);
            true
        } else {
            false
        }
    }

    pub fn get_entry(&self, key: &str) -> Option<Arc<Vec<T>>> {
        let now = Instant::now();
        let guard = self.entries.read();

        if let Some(entry) = guard.get(key)
            && !entry.is_expired(now)
        {
            return Some(entry.candidates.clone());
        }
        None
    }

    pub fn lookup(&self, input: &str) -> Option<CacheLookup<T>> {
        let now = Instant::now();
        let mut expired = Vec::new();
        let guard = self.entries.read();
        let mut best_match: Option<(String, CacheEntry<T>, bool)> = None;

        for (key, entry) in guard.iter() {
            if entry.is_expired(now) {
                expired.push(key.clone());
                continue;
            }

            debug!("cache lookup key: {}, input: {}", key, input);
            if input.starts_with(key) {
                let exact = key == input;

                if best_match
                    .as_ref()
                    .map(|(k, _, _)| key.len() > k.len())
                    .unwrap_or(true)
                {
                    best_match = Some((key.clone(), entry.clone(), exact));
                }

                if exact {
                    break;
                }
            }
        }
        drop(guard);

        debug!("best match: {:?}", best_match);

        if !expired.is_empty() {
            let mut write_guard = self.entries.write();
            for key in expired {
                write_guard.remove(&key);
            }
        }

        best_match.map(|(key, entry, exact)| {
            let candidates = if exact {
                entry.candidates.as_ref().clone()
            } else {
                let last_token = input
                    .rsplit(|c: char| c.is_whitespace())
                    .next()
                    .unwrap_or("");

                entry
                    .candidates
                    .iter()
                    .filter(|candidate| {
                        let text = candidate.completion_text();
                        text.starts_with(input)
                            || (!last_token.is_empty() && text.starts_with(last_token))
                    })
                    .cloned()
                    .collect()
            };

            CacheLookup {
                key,
                candidates,
                exact,
            }
        })
    }

    fn purge_expired_locked(entries: &mut HashMap<String, CacheEntry<T>>) {
        let now = Instant::now();
        entries.retain(|_, entry| !entry.is_expired(now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn candidate(text: &str) -> EnhancedCandidate {
        EnhancedCandidate {
            text: text.to_string(),
            description: None,
            candidate_type: super::super::integrated::CandidateType::Generic,
            priority: 0,
        }
    }

    #[test]
    fn cache_returns_exact_match() {
        let cache = CompletionCache::new(Duration::from_secs(1));
        cache.set(
            "git".to_string(),
            vec![candidate("git status"), candidate("git add")],
        );

        let result = cache.lookup("git").unwrap();
        assert!(result.exact);
        assert_eq!(result.candidates.len(), 2);
    }

    #[test]
    fn cache_filters_partial_match() {
        let cache = CompletionCache::new(Duration::from_secs(1));
        cache.set("gi".to_string(), vec![candidate("git"), candidate("gist")]);

        let result = cache.lookup("git").unwrap();
        assert!(!result.exact);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].text, "git");
    }

    #[test]
    fn cache_filters_using_last_token() {
        let cache = CompletionCache::new(Duration::from_secs(1));
        cache.set(
            "git ".to_string(),
            vec![candidate("add"), candidate("commit")],
        );

        let result = cache.lookup("git a").unwrap();
        assert!(!result.exact);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].text, "add");
    }

    #[test]
    fn cache_expires_entries() {
        let cache = CompletionCache::new(Duration::from_millis(10));
        cache.set("gi".to_string(), vec![candidate("git")]);
        std::thread::sleep(Duration::from_millis(20));
        assert!(cache.lookup("gi").is_none());
    }

    #[test]
    fn cache_extends_ttl() {
        let cache = CompletionCache::new(Duration::from_millis(30));
        cache.set("gi".to_string(), vec![candidate("git")]);
        assert!(cache.extend_ttl("gi"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(cache.lookup("gi").is_some());
    }

    #[test]
    fn cache_handles_legacy_candidates() {
        let cache: CompletionCache<Candidate> = CompletionCache::new(Duration::from_secs(1));
        cache.set(
            "ab".to_string(),
            vec![
                Candidate::Basic("abc".to_string()),
                Candidate::Basic("abd".to_string()),
            ],
        );

        let result = cache.lookup("abc").unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].get_display_name(), "abc");
    }
}
