use dsh_frecency::ItemStats;
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
