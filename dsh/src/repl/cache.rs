use dsh_frecency::ItemStats;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct HistoryCache {
    pub prefix: String,
    pub time: Option<Instant>,
    pub ttl: Duration,
    pub sorted_recent: Option<Vec<ItemStats>>,
    pub match_sorted: Option<Vec<ItemStats>>,
}

impl HistoryCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            prefix: String::new(),
            time: None,
            ttl,
            sorted_recent: None,
            match_sorted: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.sorted_recent = None;
        self.match_sorted = None;
        self.time = None;
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileContextCache {
    pub path: std::path::PathBuf,
    pub files: Arc<Vec<String>>,
    pub updated_at: Option<Instant>,
}

impl FileContextCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_valid(&self, cwd: &std::path::Path) -> bool {
        if self.path != cwd {
            return false;
        }
        if let Some(t) = self.updated_at {
            // 2 seconds validity
            if t.elapsed() < Duration::from_secs(2) {
                return true;
            }
        }
        false
    }
}
