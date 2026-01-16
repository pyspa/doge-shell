//! Frecency-based history management.
//!
//! Provides directory history with frecency scoring (frequency + recency)
//! and context-aware boosting.

use super::context::get_current_context;
use crate::db::Db;
use crate::environment;
use anyhow::Result;
use chrono::Local;
use dsh_frecency::{FrecencyStore, ItemStats, SortMethod};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

/// Message types for background frecency writer.
enum FrecencyMsg {
    Save(Arc<FrecencyStore>),
    LogVisit(String, i64, Option<String>), // path, timestamp, context
}

/// Frecency-based history for directory navigation.
pub struct FrecencyHistory {
    pub(crate) db: Option<Db>,
    pub store: Option<Arc<FrecencyStore>>,
    histories: Option<Vec<ItemStats>>,
    current_index: usize,
    pub search_word: Option<String>,
    prev_search_word: String,
    matcher: SkimMatcherV2,
    sender: Option<Sender<FrecencyMsg>>,
}

impl fmt::Debug for FrecencyHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrecencyHistory")
            .field("db", &self.db)
            .field("store", &self.store.as_ref().map(|_| "FrecencyStore"))
            .field("current_index", &self.current_index)
            .field("search_word", &self.search_word)
            .finish()
    }
}

#[allow(dead_code)]
impl Default for FrecencyHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl FrecencyHistory {
    /// Create a new empty frecency history.
    pub fn new() -> Self {
        let matcher = SkimMatcherV2::default();
        FrecencyHistory {
            db: None,
            store: Some(Arc::new(FrecencyStore::default())),
            histories: None,
            current_index: 0,
            search_word: None,
            prev_search_word: "".to_string(),
            matcher,
            sender: None,
        }
    }

    /// Create a frecency history from a database file.
    pub fn from_file(name: &str) -> Result<Self> {
        let file_path = environment::get_data_file(format!("{}.db", name).as_str())?;
        let matcher = SkimMatcherV2::default();

        let db = Db::new(file_path)?;
        let mut store = FrecencyStore::default();

        // Load from DB snapshot
        let conn = db.get_connection();
        if let Ok(mut stmt) = conn.prepare("SELECT path, score, last_accessed, access_count, half_life, context FROM directory_snapshot") {
             let rows = stmt.query_map([], |row| {
                let path: String = row.get(0)?;
                let score: f64 = row.get(1)?;
                let _last_accessed: i64 = row.get(2)?;
                let _access_count: i64 = row.get(3)?;
                let half_life: f64 = row.get(4)?;
                let context: Option<String> = row.get(5)?;

                let mut item = ItemStats::new(&path, 0.0, half_life as f32, context);
                item.set_frecency(score as f32);
                Ok(item)
            });

            if let Ok(iter) = rows {
                for item in iter.flatten() {
                    store.items.push(item);
                }
            }
        }

        drop(conn);
        store.size = store.items.len();

        Ok(FrecencyHistory {
            db: Some(db),
            store: Some(Arc::new(store)),
            histories: None,
            current_index: 0,
            search_word: None,
            prev_search_word: "".to_string(),
            matcher,
            sender: None,
        })
    }

    /// Start the background writer thread.
    pub fn start_background_writer(&mut self) {
        if let Some(db) = &self.db {
            let db_clone = db.clone();
            let (tx, rx) = mpsc::channel();
            self.sender = Some(tx);

            thread::spawn(move || {
                let mut db = db_clone;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        FrecencyMsg::Save(store) => {
                            let _ = Self::save_sync(&mut db, &store);
                        }
                        FrecencyMsg::LogVisit(path, timestamp, context) => {
                            let conn = db.get_connection();
                            let _ = conn.execute(
                                 "INSERT INTO directory_visits (path, timestamp, context) VALUES (?1, ?2, ?3)",
                                 rusqlite::params![path, timestamp, context],
                             );
                        }
                    }
                }
            });
        }
    }

    fn save_sync(db: &mut Db, store: &FrecencyStore) -> Result<()> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                 "INSERT OR REPLACE INTO directory_snapshot (path, score, last_accessed, access_count, half_life, context) 
                  VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
             )?;

            for item in &store.items {
                let serializer = dsh_frecency::ItemStatsSerializer::from(item);
                let half_life = 12.0 * 3600.0;
                stmt.execute(rusqlite::params![
                    serializer.item,
                    serializer.frecency,
                    serializer.last_accessed,
                    serializer.num_accesses,
                    half_life,
                    serializer.context,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Set the search word for filtering.
    pub fn set_search_word(&mut self, word: String) {
        if let Some(ref prev) = self.search_word {
            self.prev_search_word = prev.clone();
        }
        if word.is_empty() {
            self.search_word = None;
        } else {
            self.search_word = Some(word);
        }
    }

    #[allow(dead_code)]
    fn get(&self, index: usize) -> Option<ItemStats> {
        if let Some(ref histories) = self.histories {
            if index < histories.len() {
                let item = &histories[index];
                Some(item.clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Write a history entry.
    pub fn write_history(&mut self, history: &str) -> Result<()> {
        self.add(history);
        self.save()
    }

    /// Save the frecency store to the database.
    pub fn save(&mut self) -> Result<()> {
        if let Some(db) = &self.db
            && let Some(ref mut store) = self.store
            && store.changed
        {
            let mut conn = db.get_connection();
            let tx = conn.transaction()?;

            {
                let mut stmt = tx.prepare(
                    "INSERT OR REPLACE INTO directory_snapshot (path, score, last_accessed, access_count, half_life, context) 
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                )?;

                for item in &store.items {
                    let serializer = dsh_frecency::ItemStatsSerializer::from(item);
                    let half_life = 12.0 * 3600.0;
                    stmt.execute(rusqlite::params![
                        serializer.item,
                        serializer.frecency,
                        serializer.last_accessed,
                        serializer.num_accesses,
                        half_life,
                        serializer.context,
                    ])?;
                }
            }

            tx.commit()?;

            let store_mut = Arc::make_mut(store);
            store_mut.changed = false;
        }
        Ok(())
    }

    /// Reload the frecency store from the database.
    pub fn reload(&mut self) -> Result<()> {
        if self.db.is_none() || self.store.is_none() {
            return Ok(());
        }

        let db = self.db.as_ref().unwrap();
        let conn = db.get_connection();
        let mut stmt = conn.prepare("SELECT path, score, last_accessed, access_count, half_life, context FROM directory_snapshot")?;

        let rows = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            let last_accessed: f64 = row.get(2)?;
            let access_count: i64 = row.get(3)?;
            let half_life: f64 = row.get(4)?;
            let context: Option<String> = row.get(5)?;
            Ok((path, score, last_accessed, access_count, half_life, context))
        })?;

        let store = self.store.as_mut().unwrap();
        let store_mut = Arc::make_mut(store);

        for row in rows {
            match row {
                Ok((path, score, _last_accessed, _access_count, half_life, context)) => {
                    match store_mut.items.binary_search_by(|i| i.item.cmp(&path)) {
                        Ok(_idx) => {
                            // Exists - skip for now
                        }
                        Err(idx) => {
                            let mut new_item =
                                ItemStats::new(&path, 0.0, half_life as f32, context);
                            new_item.set_frecency(score as f32);
                            store_mut.items.insert(idx, new_item);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Reload Row Error: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Save in background using the writer thread.
    pub fn save_background(&mut self) {
        if let Some(ref mut store) = self.store
            && store.changed
        {
            if let Some(sender) = &self.sender {
                let _ = sender.send(FrecencyMsg::Save(Arc::clone(store)));
                Arc::make_mut(store).changed = false;
            } else {
                let _ = self.save();
            }
        }
    }

    /// Reset the navigation index.
    pub fn reset_index(&mut self) {
        if let Some(ref histories) = self.histories {
            self.current_index = histories.len();
        } else {
            self.current_index = 0;
        }
    }

    /// Search for a path with the given prefix.
    pub fn search_prefix(&self, pattern: &str) -> Option<String> {
        let ctx = get_current_context();
        self.search_prefix_with_context(pattern, ctx.as_deref())
    }

    /// Force the store to be marked as changed.
    pub fn force_changed(&mut self) {
        if let Some(ref mut store) = self.store {
            Arc::make_mut(store).changed = true;
        }
    }

    /// Add a new entry to the frecency history.
    pub fn add(&mut self, history: &str) {
        if let Some(ref mut store) = self.store {
            let ctx = get_current_context();
            Arc::make_mut(store).add(history, ctx.clone());

            // Log to directory_visits
            if let Some(sender) = &self.sender {
                let now = Local::now().timestamp();
                let _ = sender.send(FrecencyMsg::LogVisit(history.to_string(), now, ctx));
            } else if let Some(db) = &self.db {
                let conn = db.get_connection();
                let now = Local::now().timestamp();
                let _ = conn.execute(
                    "INSERT INTO directory_visits (path, timestamp, context) VALUES (?1, ?2, ?3)",
                    rusqlite::params![history, now, ctx],
                );
            }
        }
    }

    /// Search for a recent path with the given prefix.
    pub fn search_recent_prefix(&self, pattern: &str) -> Option<String> {
        if let Some(ref store) = self.store {
            let range = store.search_prefix_range(pattern);
            store
                .items
                .get(range)?
                .iter()
                .max_by(|a, b| a.cmp_recent(b))
                .map(|item| item.item.clone())
        } else {
            None
        }
    }

    /// Prune old entries from the store.
    pub fn prune(&mut self) {
        if let Some(ref mut store) = self.store {
            Arc::make_mut(store).prune();
        }
    }

    /// Search for a path with context boosting.
    pub fn search_prefix_with_context(
        &self,
        pattern: &str,
        context: Option<&str>,
    ) -> Option<String> {
        if let Some(ref store) = self.store {
            let range = store.search_prefix_range(pattern);
            store
                .items
                .get(range)?
                .iter()
                .max_by(|a, b| a.cmp_frecent_with_context(b, context))
                .map(|item| item.item.clone())
        } else {
            None
        }
    }

    /// Sort items by fuzzy match score.
    pub fn sort_by_match(&self, pattern: &str) -> Vec<ItemStats> {
        let ctx = get_current_context();
        self.sort_by_match_with_context(pattern, ctx.as_deref())
    }

    /// Sort items by fuzzy match score with context boosting.
    pub fn sort_by_match_with_context(
        &self,
        pattern: &str,
        context: Option<&str>,
    ) -> Vec<ItemStats> {
        let Some(ref store) = self.store else {
            return Vec::new();
        };

        let estimated_matches = store.items.len() / 4;
        let mut results = Vec::with_capacity(estimated_matches.max(16));

        for item in store.items.iter() {
            if let Some((score, index)) = self.matcher.fuzzy_indices(&item.item, pattern)
                && score > 25
            {
                let mut item = item.clone();
                item.match_score = score;
                item.match_index = index;
                results.push(item);
            }
        }

        results.sort_by(|a, b| a.cmp_match_score_with_context(b, context).reverse());
        results
    }

    /// Get sorted items by the given method.
    pub fn sorted(&self, sort_method: &SortMethod) -> Vec<ItemStats> {
        if let Some(ref store) = self.store {
            store.sorted(sort_method)
        } else {
            Vec::new()
        }
    }

    /// Print match scores for debugging.
    pub fn show_score(&self, pattern: &str) {
        for res in self.sort_by_match(pattern) {
            println!("'{}' match_score:{:?}", &res.item, res.match_score);
        }
    }
}
