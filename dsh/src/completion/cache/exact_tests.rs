use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct CountedCandidate {
    text: String,
    clones: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}
impl Clone for CountedCandidate {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            text: self.text.clone(),
            clones: self.clones.clone(),
            reads: self.reads.clone(),
        }
    }
}
impl CacheableCandidate for CountedCandidate {
    fn completion_text(&self) -> &str {
        self.reads.fetch_add(1, Ordering::Relaxed);
        &self.text
    }
}
fn fixture() -> (
    CompletionCache<CountedCandidate>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let clones = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let cache = CompletionCache::new(Duration::from_secs(60));
    let candidates = ["feature/z", "feature/a"]
        .into_iter()
        .map(|text| CountedCandidate {
            text: text.into(),
            clones: clones.clone(),
            reads: reads.clone(),
        })
        .collect();
    cache.set_scoped(7, "git ".into(), candidates);
    (cache, clones, reads)
}
#[test]
fn scoped_exact_preserves_order_and_clones_only_the_exact_snapshot() {
    let (cache, clones, reads) = fixture();
    let expiry = cache.scoped_expiration(7, "git ").unwrap();
    let hit = cache.lookup_scoped_exact(7, "git ").unwrap();
    assert_eq!(cache.scoped_expiration(7, "git "), Some(expiry));
    assert!(hit.exact);
    assert_eq!(hit.key, "git ");
    assert_eq!(
        hit.candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["feature/z", "feature/a"]
    );
    assert_eq!(clones.load(Ordering::Relaxed), 2);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
}
#[test]
fn scoped_exact_misses_do_not_scan_or_clone_candidates() {
    let (cache, clones, reads) = fixture();
    for (scope, input) in [(7, "git f"), (7, "missing"), (8, "git ")] {
        assert!(cache.lookup_scoped_exact(scope, input).is_none());
    }
    assert_eq!(clones.load(Ordering::Relaxed), 0);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    // Confirm the counters detect the discarded work in the unchanged legacy API.
    let prefix = cache.lookup_scoped(7, "git f").unwrap();
    assert!(!prefix.exact);
    assert_eq!(prefix.candidates.len(), 2);
    assert_eq!(clones.load(Ordering::Relaxed), 2);
    assert_eq!(reads.load(Ordering::Relaxed), 2);
}
#[test]
fn scoped_exact_expired_entry_does_not_fall_back_to_a_live_prefix() {
    let (cache, clones, reads) = fixture();
    cache.set_scoped(7, "git f".into(), Vec::new());
    cache
        .scoped_entries
        .write()
        .get_mut(&7)
        .unwrap()
        .get_mut("git f")
        .unwrap()
        .expires_at = Instant::now() - Duration::from_secs(1);
    assert!(cache.lookup_scoped_exact(7, "git f").is_none());
    assert_eq!(clones.load(Ordering::Relaxed), 0);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
}
#[test]
fn scoped_exact_returns_empty_snapshots_without_extending_ttl() {
    let cache = CompletionCache::<CountedCandidate>::new(Duration::from_secs(60));
    cache.set_scoped(7, "".into(), Vec::new());
    let expiry = cache.scoped_expiration(7, "").unwrap();
    for _ in 0..3 {
        let hit = cache.lookup_scoped_exact(7, "").unwrap();
        assert!(hit.exact);
        assert!(hit.candidates.is_empty());
        assert_eq!(cache.scoped_expiration(7, ""), Some(expiry));
    }
}

#[test]
fn scoped_exact_expired_nonempty_snapshot_is_not_copied() {
    let (cache, clones, reads) = fixture();
    cache
        .scoped_entries
        .write()
        .get_mut(&7)
        .unwrap()
        .get_mut("git ")
        .unwrap()
        .expires_at = Instant::now() - Duration::from_secs(1);
    assert!(cache.lookup_scoped_exact(7, "git ").is_none());
    assert_eq!(clones.load(Ordering::Relaxed), 0);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
}
