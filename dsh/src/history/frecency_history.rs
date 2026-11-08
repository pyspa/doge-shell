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
    Delete(Vec<String>),
    Reload {
        base_revision: u64,
        base_store: Arc<FrecencyStore>,
        complete: FrecencyReloadCallback,
    },
}

/// Rows of `directory_visits` kept after each insert. Nothing reads the log
/// yet, so it only has to stay bounded.
const MAX_DIRECTORY_VISITS: i64 = 10_000;

/// `last_accessed` is saved as absolute epoch seconds. Builds before that
/// saved a session-relative offset, which cannot be restored; such small
/// values are left at the load time, as before.
const MIN_ABSOLUTE_ACCESS_TIME: f64 = 1_000_000_000.0;

const SNAPSHOT_SELECT: &str =
    "SELECT path, score, last_accessed, access_count, half_life, context FROM directory_snapshot";

type FrecencyReloadCallback = Box<dyn FnOnce(Result<FrecencyReloadSnapshot>) + Send + 'static>;

pub(crate) struct FrecencyReloadSnapshot {
    base_revision: u64,
    store: Arc<FrecencyStore>,
}

impl fmt::Debug for FrecencyReloadSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrecencyReloadSnapshot")
            .field("base_revision", &self.base_revision)
            .field("entries", &self.store.items.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrecencyReloadApply {
    Applied,
    Stale,
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
    revision: u64,
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
            revision: 0,
        }
    }

    /// Create a frecency history from a database file.
    pub fn from_file(name: &str) -> Result<Self> {
        let file_path = environment::get_data_file(format!("{}.db", name).as_str())?;
        Self::from_db(Db::new(file_path)?)
    }

    /// Create a frecency history backed by an open database.
    pub(crate) fn from_db(db: Db) -> Result<Self> {
        let matcher = SkimMatcherV2::default();
        let mut store = FrecencyStore::default();

        // Load from DB snapshot
        match read_snapshot_items(&db.get_connection(), &store) {
            Ok(items) => store.items = items,
            Err(err) => tracing::warn!("frecency: failed to load directory snapshot: {err}"),
        }

        // Rows come back in rowid order, and `INSERT OR REPLACE` reassigns
        // rowids on every save, so re-sort by name to restore the invariant
        // `FrecencyStore::get` / `search_prefix_range` rely on (see
        // dsh-frecency/src/store.rs).
        store.items.sort_by(|a, b| a.item.cmp(&b.item));
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
            revision: 0,
        })
    }

    /// Start the background writer thread.
    pub fn start_background_writer(&mut self) {
        if let Some(db) = &self.db {
            let db_clone = db.clone();
            let (tx, rx) = mpsc::channel();
            self.sender = Some(tx);

            thread::spawn(move || {
                let db = db_clone;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        FrecencyMsg::Save(store) => {
                            let _ = Self::write_snapshot(&db, &store);
                        }
                        FrecencyMsg::LogVisit(path, timestamp, context) => {
                            let conn = db.get_connection();
                            log_visit(
                                &conn,
                                &path,
                                timestamp,
                                context.as_deref(),
                                MAX_DIRECTORY_VISITS,
                            );
                        }
                        FrecencyMsg::Delete(paths) => {
                            let _ = Self::delete_snapshot_paths(&db, &paths);
                        }
                        FrecencyMsg::Reload {
                            base_revision,
                            base_store,
                            complete,
                        } => {
                            complete(Self::load_reload_snapshot(&db, base_revision, base_store));
                        }
                    }
                }
            });
        }
    }

    /// Write the store as of now: scores are the current (decayed) values
    /// and `last_accessed` is absolute, so a later load needs no session
    /// reference time.
    fn write_snapshot(db: &Db, store: &FrecencyStore) -> Result<()> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO directory_snapshot (path, score, last_accessed, access_count, half_life, context)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;

            for item in &store.items {
                let num_accesses = item.num_accesses();
                stmt.execute(rusqlite::params![
                    item.item,
                    f64::from(finite_score(item.get_frecency(), num_accesses)),
                    item.last_accessed_time(),
                    num_accesses,
                    f64::from(store.half_life),
                    item.context,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn delete_snapshot_paths(db: &Db, paths: &[String]) -> Result<()> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM directory_snapshot WHERE path = ?1")?;
            for path in paths {
                stmt.execute([path])?;
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

    // Indexed accessor kept as part of the history query API.
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
            Self::write_snapshot(db, store)?;

            let store_mut = Arc::make_mut(store);
            store_mut.changed = false;
        }
        Ok(())
    }

    /// Reload the frecency store from the database.
    pub fn reload(&mut self) -> Result<()> {
        let (Some(db), Some(store)) = (self.db.clone(), self.store.clone()) else {
            return Ok(());
        };
        let snapshot = Self::load_reload_snapshot(&db, self.revision, store)?;
        let _ = self.apply_reload_snapshot(snapshot);
        Ok(())
    }

    pub(crate) fn request_reload<F>(&self, complete: F) -> bool
    where
        F: FnOnce(Result<FrecencyReloadSnapshot>) + Send + 'static,
    {
        let (Some(sender), Some(store)) = (&self.sender, &self.store) else {
            return false;
        };
        sender
            .send(FrecencyMsg::Reload {
                base_revision: self.revision,
                base_store: Arc::clone(store),
                complete: Box::new(complete),
            })
            .is_ok()
    }

    fn load_reload_snapshot(
        db: &Db,
        base_revision: u64,
        base_store: Arc<FrecencyStore>,
    ) -> Result<FrecencyReloadSnapshot> {
        let mut store = (*base_store).clone();
        let loaded = read_snapshot_items(&db.get_connection(), &store)?;

        for item in loaded {
            if let Err(index) = store
                .items
                .binary_search_by(|existing| existing.item.cmp(&item.item))
            {
                store.items.insert(index, item);
            }
        }
        store.size = store.items.len();

        Ok(FrecencyReloadSnapshot {
            base_revision,
            store: Arc::new(store),
        })
    }

    pub(crate) fn apply_reload_snapshot(
        &mut self,
        snapshot: FrecencyReloadSnapshot,
    ) -> FrecencyReloadApply {
        if self.revision != snapshot.base_revision {
            return FrecencyReloadApply::Stale;
        }
        self.store = Some(snapshot.store);
        self.histories = None;
        self.reset_index();
        self.bump_revision();
        FrecencyReloadApply::Applied
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
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
            self.bump_revision();
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
                log_visit(&conn, history, now, ctx.as_deref(), MAX_DIRECTORY_VISITS);
            }
            self.bump_revision();
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

    /// Prune entries whose directory no longer exists, in memory and in the
    /// database: saves only upsert, so a row left behind would come back on
    /// the next load or reload.
    pub fn prune(&mut self) {
        let Some(ref mut store) = self.store else {
            return;
        };
        let store = Arc::make_mut(store);
        let before: Vec<String> = store.items.iter().map(|item| item.item.clone()).collect();
        store.prune();
        let removed: Vec<String> = before
            .into_iter()
            .filter(|path| {
                store
                    .items
                    .binary_search_by(|item| item.item.as_str().cmp(path))
                    .is_err()
            })
            .collect();
        if removed.is_empty() {
            return;
        }
        self.bump_revision();
        // Through the writer when there is one, so a queued save of the old
        // store cannot re-insert the rows after they are deleted.
        if let Some(sender) = &self.sender {
            let _ = sender.send(FrecencyMsg::Delete(removed));
        } else if let Some(db) = &self.db
            && let Err(err) = Self::delete_snapshot_paths(db, &removed)
        {
            tracing::warn!("frecency: failed to delete pruned paths: {err}");
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
            println!("'{}' match_score:{:?}", res.item, res.match_score);
        }
    }
}

/// Read every snapshot row as an item relative to `store`'s reference time.
/// A row that cannot be decoded is skipped rather than failing the load.
fn read_snapshot_items(
    conn: &rusqlite::Connection,
    store: &FrecencyStore,
) -> rusqlite::Result<Vec<ItemStats>> {
    let mut stmt = conn.prepare(SNAPSHOT_SELECT)?;
    let rows = stmt.query_map([], |row| {
        // `last_accessed` is REAL in practice despite the INTEGER column
        // affinity; reading it as f64 accepts both storage classes.
        let path: String = row.get(0)?;
        let score: f64 = row.get(1)?;
        let last_accessed: f64 = row.get(2)?;
        let access_count: i64 = row.get(3)?;
        let half_life: f64 = row.get(4)?;
        let context: Option<String> = row.get(5)?;

        let half_life = if half_life.is_finite() && half_life > 0.0 {
            half_life as f32
        } else {
            store.half_life
        };
        let access_count = i32::try_from(access_count.max(0)).unwrap_or(i32::MAX);
        let mut item = ItemStats::new(&path, store.reference_time, half_life, context);
        item.update_num_accesses(access_count);
        if last_accessed.is_finite() && last_accessed >= MIN_ABSOLUTE_ACCESS_TIME {
            item.update_last_access(last_accessed);
        }
        item.set_frecency(finite_score(score as f32, access_count));
        Ok(item)
    })?;

    let mut items = Vec::new();
    for row in rows {
        match row {
            Ok(item) => items.push(item),
            Err(err) => tracing::warn!("frecency: skipping unreadable snapshot row: {err}"),
        }
    }
    Ok(items)
}

/// Older builds saved the time-scaled internal score, which grew on every
/// restart until it overflowed. Fall back to the access count for a score
/// that is not a finite, non-negative number.
fn finite_score(score: f32, access_count: i32) -> f32 {
    if score.is_finite() && score >= 0.0 {
        score
    } else {
        access_count.max(1) as f32
    }
}

/// Append a visit and drop all but the latest `keep` rows. Ids are
/// AUTOINCREMENT, so they only grow and the cut-off is a primary-key range.
fn log_visit(
    conn: &rusqlite::Connection,
    path: &str,
    timestamp: i64,
    context: Option<&str>,
    keep: i64,
) {
    let inserted = conn.execute(
        "INSERT INTO directory_visits (path, timestamp, context) VALUES (?1, ?2, ?3)",
        rusqlite::params![path, timestamp, context],
    );
    if let Err(err) = inserted {
        tracing::debug!("frecency: failed to log directory visit: {err}");
        return;
    }
    let _ = conn.execute(
        "DELETE FROM directory_visits WHERE id <= last_insert_rowid() - ?1",
        [keep],
    );
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod background_reload_tests {
    use super::*;

    #[test]
    fn stale_reload_snapshot_does_not_overwrite_local_visit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::new(dir.path().join("frecency.db")).unwrap();
        {
            let conn = db.get_connection();
            conn.execute(
                "INSERT INTO directory_snapshot \
                 (path, score, last_accessed, access_count, half_life, context) \
                 VALUES ('/remote', 1.0, 1, 1, 43200.0, NULL)",
                [],
            )
            .unwrap();
        }

        let mut history = FrecencyHistory::new();
        history.db = Some(db.clone());
        let snapshot = FrecencyHistory::load_reload_snapshot(
            &db,
            history.revision,
            history.store.clone().unwrap(),
        )
        .unwrap();
        history.add("/local");

        assert_eq!(
            history.apply_reload_snapshot(snapshot),
            FrecencyReloadApply::Stale
        );
        assert!(
            history
                .store
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|item| item.item == "/local")
        );
    }
}
