use crate::db::Db;
use crate::environment;
use anyhow::Result;
use chrono::Local;
use dsh_frecency::{FrecencyStore, ItemStats, SortMethod};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

fn get_current_context() -> Option<String> {
    // Try to get git root
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() {
            return Some(root);
        }
    }

    // Fallback to current directory
    std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Entry {
    pub entry: String,
    pub when: i64,
    pub count: i64,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct History {
    db: Option<Db>,
    histories: Vec<Entry>,
    size: usize,
    current_index: usize,
    pub search_word: Option<String>,
    sender: Option<Sender<HistoryMsg>>,
}

enum HistoryMsg {
    WriteBatch(Vec<(String, i64)>, Option<String>), // entries, context
}

#[allow(dead_code)]
impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        History {
            db: None,
            histories: Vec::new(),
            size: 10000,
            current_index: 0,

            search_word: None,
            sender: None,
        }
    }

    pub fn from_file(name: &str) -> Result<Self> {
        let file_path = environment::get_data_file(format!("{}.db", name).as_str())?;

        let db = Db::new(file_path)?;

        Ok(History {
            db: Some(db),
            histories: Vec::new(),
            size: 10000,
            current_index: 0,

            search_word: None,
            sender: None,
        })
    }

    fn get(&self, index: usize) -> Option<String> {
        if index < self.histories.len() {
            let entry = &self.histories[index].entry;
            Some(entry.to_string())
        } else {
            None
        }
    }

    pub fn back(&mut self) -> Option<String> {
        if self.current_index > 0 {
            self.current_index -= 1;
            match &self.search_word {
                Some(word) => {
                    while let Some(entry) = self.get(self.current_index) {
                        if entry.starts_with(word) {
                            return Some(entry);
                        }
                        self.current_index -= 1;
                        if self.current_index == 0 {
                            break;
                        }
                    }
                    None
                }
                None => self.get(self.current_index),
            }
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<String> {
        if self.current_index + 1 < self.histories.len() {
            self.current_index += 1;
            match &self.search_word {
                Some(word) => {
                    while let Some(entry) = self.get(self.current_index) {
                        if entry.starts_with(word) {
                            return Some(entry);
                        }
                        self.current_index += 1;
                    }
                    None
                }
                None => self.get(self.current_index),
            }
        } else {
            None
        }
    }

    pub fn reset_index(&mut self) {
        self.current_index = self.histories.len();
    }

    pub fn at_end(&self) -> bool {
        self.current_index == self.histories.len()
    }

    pub fn at_latest_entry(&self) -> bool {
        self.current_index == self.histories.len().saturating_sub(1)
    }

    pub fn load(&mut self) -> Result<usize> {
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            // Use subquery to get recent 10000 items, then sort by timestamp ASC for correct history order
            // Note: command is UNIQUE in command_history, so usage of LIMIT is safe without GROUP BY
            let mut stmt = conn.prepare(
                "SELECT command, timestamp, count 
                 FROM (
                    SELECT command, timestamp, count 
                    FROM command_history 
                    ORDER BY timestamp DESC 
                    LIMIT 10000
                 ) 
                 ORDER BY timestamp ASC",
            )?;

            let rows = stmt.query_map([], |row| {
                Ok(Entry {
                    entry: row.get(0)?,
                    when: row.get(1)?,
                    count: row.get(2).unwrap_or(1),
                })
            })?;

            self.histories.clear();

            // Direct collect as database guarantees uniqueness
            for r in rows.flatten() {
                self.histories.push(r);
            }

            self.current_index = self.histories.len();
        }
        Ok(self.histories.len())
    }

    pub fn start_background_writer(&mut self) {
        if let Some(db) = self.db.take() {
            let (tx, rx) = mpsc::channel();
            self.sender = Some(tx);

            thread::spawn(move || {
                let mut db = db;
                // Keep connection open in this thread
                while let Ok(msg) = rx.recv() {
                    match msg {
                        HistoryMsg::WriteBatch(entries, context) => {
                            let _ = Self::write_batch_sync(&mut db, entries, context);
                        }
                    }
                }
            });
        }
    }

    // Helper method for synchronous writing (moved from write_batch)
    fn write_batch_sync(
        db: &mut Db,
        entries: Vec<(String, i64)>,
        context: Option<String>,
    ) -> Result<()> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;

        {
            let mut upsert_stmt = tx.prepare(
                "INSERT INTO command_history (command, timestamp, context, count) 
                  VALUES (?1, ?2, ?3, 1)
                  ON CONFLICT(command) DO UPDATE SET 
                      count = count + 1,
                      timestamp = excluded.timestamp,
                      context = excluded.context
                  RETURNING count",
            )?;

            for (cmd, when) in &entries {
                let _count: i64 = upsert_stmt
                    .query_row(rusqlite::params![cmd, when, context], |row| row.get(0))
                    .unwrap_or(1);
            }
        }
        tx.commit()?;
        Ok(())
    }

    // Helper methods open/close removed as they are no longer needed
    #[allow(dead_code)]
    fn open(&mut self) -> Result<&mut History> {
        Ok(self)
    }
    #[allow(dead_code)]
    fn close(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn write_history(&mut self, history: &str) -> Result<()> {
        self.write_batch(vec![(history.to_string(), Local::now().timestamp())])
    }

    pub fn write_batch(&mut self, entries: Vec<(String, i64)>) -> Result<()> {
        let context = get_current_context();

        // 1. Update in-memory history immediately
        for (cmd, when) in &entries {
            // Check if exists and update/move to end
            let mut count = 1;
            if let Some(pos) = self.histories.iter().position(|e| e.entry == *cmd) {
                count = self.histories[pos].count + 1;
                self.histories.remove(pos);
            }
            self.histories.push(Entry {
                entry: cmd.clone(),
                when: *when,
                count,
            });
        }
        self.reset_index();

        // 2. Persist
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::WriteBatch(entries, context));
        } else if let Some(db) = &mut self.db {
            // Fallback sync
            let _ = Self::write_batch_sync(db, entries, context);
        }
        Ok(())
    }

    pub fn search_first(&self, word: &str) -> Option<&str> {
        for hist in self.histories.iter().rev() {
            if hist.entry.starts_with(word) {
                return Some(&hist.entry);
            }
        }
        None
    }

    pub fn get_recent_context(&self, limit: usize) -> Vec<String> {
        self.histories
            .iter()
            .rev()
            .take(limit)
            .map(|e| e.entry.clone())
            .collect()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Entry> {
        self.histories.iter()
    }

    #[cfg(test)]
    pub fn add_test_entry(&mut self, entry: &str) {
        self.histories.push(Entry {
            entry: entry.to_string(),
            when: Local::now().timestamp(),
            count: 1,
        });
        self.size = self.histories.len();
        self.current_index = self.histories.len();
    }
}

// #[derive(Debug)] // Removed derive as matches/store don't implement Debug
pub struct FrecencyHistory {
    db: Option<Db>,
    pub store: Option<Arc<FrecencyStore>>,
    histories: Option<Vec<ItemStats>>,
    current_index: usize,
    pub search_word: Option<String>,
    prev_search_word: String,
    matcher: SkimMatcherV2,
    sender: Option<Sender<FrecencyMsg>>,
}

enum FrecencyMsg {
    Save(Arc<FrecencyStore>),
    LogVisit(String, i64, Option<String>), // path, timestamp, context
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
    pub fn new() -> Self {
        let matcher = SkimMatcherV2::default();
        FrecencyHistory {
            db: None,
            store: None,
            histories: None,
            current_index: 0,
            search_word: None,
            prev_search_word: "".to_string(),
            matcher,
            sender: None,
        }
    }

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
                // Unused but read for schema compatibility
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

        // Explicitly drop usage of db to allow move
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

    pub fn start_background_writer(&mut self) {
        if let Some(db) = self.db.take() {
            let (tx, rx) = mpsc::channel();
            self.sender = Some(tx);

            thread::spawn(move || {
                let mut db = db;
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

    // ... (backward/forward/search methods remain largely same,
    // but save() and write_history() need to change)

    pub fn write_history(&mut self, history: &str) -> Result<()> {
        self.add(history);
        self.save()
    }

    pub fn save(&mut self) -> Result<()> {
        if let Some(db) = &self.db
            && let Some(ref mut store) = self.store
            && store.changed
        {
            let mut conn = db.get_connection();

            // Use transaction for consistency and performance
            let tx = conn.transaction()?;

            {
                let mut stmt = tx.prepare(
                    "INSERT OR REPLACE INTO directory_snapshot (path, score, last_accessed, access_count, half_life, context) 
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                )?;

                for item in &store.items {
                    let serializer = dsh_frecency::ItemStatsSerializer::from(item);

                    // Note: half_life matches the default 12 hours (43200s) usually used in frecency
                    // Since serializer might not expose it, we use a default or calculate if possible.
                    // For now, we assume a standard half-life if not available.
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

    /// Save command but now using SQLite
    pub fn save_background(&mut self) {
        if let Some(ref mut store) = self.store
            && store.changed
        {
            if let Some(sender) = &self.sender {
                let _ = sender.send(FrecencyMsg::Save(Arc::clone(store)));
                Arc::make_mut(store).changed = false;
            } else {
                // Fallback
                let _ = self.save();
            }
        }
    }

    pub fn reset_index(&mut self) {
        if let Some(ref histories) = self.histories {
            self.current_index = histories.len();
        } else {
            self.current_index = 0;
        }
    }

    pub fn search_prefix(&self, pattern: &str) -> Option<String> {
        // Use frecency-based search (with context if available)
        let ctx = get_current_context();
        self.search_prefix_with_context(pattern, ctx.as_deref())
    }

    pub fn force_changed(&mut self) {
        if let Some(ref mut store) = self.store {
            Arc::make_mut(store).changed = true;
        }
    }

    pub fn add(&mut self, history: &str) {
        if let Some(ref mut store) = self.store {
            let ctx = get_current_context();
            Arc::make_mut(store).add(history, ctx.clone());

            // Log to directory_visits (Append Only)
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

    pub fn prune(&mut self) {
        if let Some(ref mut store) = self.store {
            Arc::make_mut(store).prune();
        }
    }

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

    pub fn sort_by_match(&self, pattern: &str) -> Vec<ItemStats> {
        let ctx = get_current_context();
        self.sort_by_match_with_context(pattern, ctx.as_deref())
    }

    pub fn sort_by_match_with_context(
        &self,
        pattern: &str,
        context: Option<&str>,
    ) -> Vec<ItemStats> {
        let Some(ref store) = self.store else {
            return Vec::new();
        };

        // Pre-allocate with estimated capacity (assume ~25% match rate)
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

    pub fn sorted(&self, sort_method: &SortMethod) -> Vec<ItemStats> {
        if let Some(ref store) = self.store {
            store.sorted(sort_method)
        } else {
            Vec::new()
        }
    }

    pub fn show_score(&self, pattern: &str) {
        for res in self.sort_by_match(pattern) {
            println!("'{}' match_score:{:?}", &res.item, res.match_score);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_new() {
        init();
        let history = History::from_file("dsh_cmd_history").unwrap();
        assert!(history.db.is_some());
    }

    #[test]
    fn test_open() -> Result<()> {
        init();
        let mut history = History::from_file("dsh_cmd_history")?;
        let history = history.open()?;
        history.close()
    }

    #[test]
    fn test_write_batch() -> Result<()> {
        init();
        // Clear test file if exists
        let test_name = "dsh_test_batch";
        if let Ok(path) = environment::get_data_file(format!("{}.db", test_name).as_str()) {
            let _ = std::fs::remove_file(path);
        }

        let mut history = History::from_file(test_name)?;

        let now = Local::now().timestamp();
        let entries = vec![
            ("ls".to_string(), now - 10),
            ("cd".to_string(), now - 5),
            ("ls".to_string(), now),
        ];

        history.write_batch(entries)?;

        assert_eq!(history.histories.len(), 2); // "ls" should be deduplicated

        let ls_entry = history.histories.iter().find(|e| e.entry == "ls").unwrap();
        assert_eq!(ls_entry.count, 2);
        assert_eq!(ls_entry.when, now); // Should have the latest timestamp

        let cd_entry = history.histories.iter().find(|e| e.entry == "cd").unwrap();
        assert_eq!(cd_entry.count, 1);

        Ok(())
    }

    #[test]
    // #[ignore]
    fn test_load() -> Result<()> {
        init();
        let mut history = History::from_file("dsh_cmd_history")?;
        let s = history.load()?;
        tracing::debug!("loaded {:?}", s);
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_back() -> Result<()> {
        init();
        let cmd1 = "docker";
        let cmd2 = "docker-compose";

        let mut history = History::from_file("dsh_cmd_history")?;

        let s = history.load()?;
        tracing::debug!("loaded {:?}", s);

        history.write_history(cmd1)?;
        history.write_history(cmd2)?;

        if let Some(h) = history.back() {
            assert_eq!(cmd2, h);
        } else {
            panic!("failed read history");
        }
        if let Some(h) = history.back() {
            assert_eq!(cmd1, h);
        } else {
            panic!("failed read history");
        }

        Ok(())
    }

    #[test]
    #[ignore]
    fn frecency() -> Result<()> {
        init();
        let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
        history.add("git");
        history.add("git");
        std::thread::sleep(std::time::Duration::from_millis(100));
        history.add("git checkout");
        let recent = history.sorted(&SortMethod::Recent);
        assert_eq!(recent[0].item, "git checkout");
        assert_eq!(recent[1].item, "git");

        let frequent = history.sorted(&SortMethod::Frequent);
        assert_eq!(frequent[0].item, "git");
        assert_eq!(frequent[1].item, "git checkout");

        let first = history.search_prefix("gi").unwrap();
        assert_eq!(first, "git checkout");

        history.add("git checkout origin master");
        history.add("git config --list");
        history.add("git switch config");
        history.show_score("gc");

        Ok(())
    }

    #[test]
    fn print_item() -> Result<()> {
        init();
        let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
        history.add("git status");
        history.add("git checkout");

        let vec = history.sort_by_match("gsta");
        let mut out = std::io::stdout().lock();
        for item in vec {
            item.print(&mut out);
        }

        Ok(())
    }

    #[test]
    fn test_frecency_completion() -> Result<()> {
        init();
        // Use a temporary file for this test
        let temp_dir = tempfile::tempdir()?;
        let _file_path = temp_dir.path().join("frecency_test_history");

        let mut history = FrecencyHistory::new();
        // history.path = Some(file_path.clone());
        history.store = Some(Arc::new(dsh_frecency::FrecencyStore::default()));

        // Add "frequent_cmd" 5 times
        for _ in 0..5 {
            history.add("frequent_cmd");
        }

        // Add "frequent_but_old" 5 times, but simulate time passing if possible?
        // dsh_frecency uses system time, so consistent time manipulation is hard without mocking.
        // But assuming same timeframe:

        // Add "recent_cmd" once
        history.add("recent_cmd");

        // "f frequent_cmd" (5 accesses) vs "r recent_cmd" (1 access)

        // If we search for empty prefix, or common prefix if they had one.
        // Let's use common prefix "cmd"
        history.add("cmd_frequent"); // 1
        history.add("cmd_frequent"); // 2
        history.add("cmd_frequent"); // 3
        history.add("cmd_recent"); // 1 (most recent)

        // Search for "cmd"
        // cmd_frequent: score ~ 3
        // cmd_recent: score ~ 1 (but decent recency)
        // Frecency should favor cmd_frequent if weights are standard.

        let result = history.search_prefix("cmd");
        assert_eq!(result, Some("cmd_frequent".to_string()));

        // Search for "cmd_r" should still find cmd_recent
        let result_recent = history.search_prefix("cmd_r");
        assert_eq!(result_recent, Some("cmd_recent".to_string()));

        Ok(())
    }

    #[test]
    fn test_save_no_path() -> Result<()> {
        init();
        let mut history = FrecencyHistory::new();
        // Path is None by default
        assert!(history.db.is_none());

        // Should not panic
        history.save()?;

        Ok(())
    }

    #[test]
    fn test_context_aware_boosting() -> Result<()> {
        init();
        let temp_dir = tempfile::tempdir()?;
        let dir_a = temp_dir.path().join("a");
        let dir_b = temp_dir.path().join("b");
        // We don't need to create real directories since we pass strings
        // std::fs::create_dir(&dir_a)?;
        // std::fs::create_dir(&dir_b)?;
        let dir_a_str = dir_a.to_string_lossy().to_string();
        let dir_b_str = dir_b.to_string_lossy().to_string();

        let _history_file = temp_dir.path().join("history_context");
        let mut history = FrecencyHistory::new();
        let _path = PathBuf::from("history_context.db");
        // We can't easily mock DB here without real file or in-memory, but this test focused on store.
        // So we keep db None.
        history.store = Some(Arc::new(dsh_frecency::FrecencyStore::default()));

        // Simulate usage in Dir A
        if let Some(ref mut store) = history.store {
            let store_mut = Arc::make_mut(store);
            store_mut.add("cmd_common", Some(dir_a_str.clone()));
            store_mut.add("cmd_unique_a", Some(dir_a_str.clone()));

            // Simulate usage in Dir B
            store_mut.add("cmd_common", Some(dir_b_str.clone())); // common cmd used in both
            store_mut.add("cmd_unique_b", Some(dir_b_str.clone()));
        }

        // Back to Dir A context
        // Try searching in context A
        // "cmd_unique_a" (score 1.0 * 2.0 = 2.0) vs "cmd_unique_b" (score 1.0 * 1.0 = 1.0)
        let result = history.search_prefix_with_context("cmd_unique", Some(&dir_a_str));
        assert_eq!(result, Some("cmd_unique_a".to_string()));

        // Now context B
        let result_b = history.search_prefix_with_context("cmd_unique", Some(&dir_b_str));
        assert_eq!(result_b, Some("cmd_unique_b".to_string()));

        Ok(())
    }
    #[test]
    fn test_arc_cow_behavior() -> Result<()> {
        init();
        let mut history = FrecencyHistory::new();
        // Create initial store
        let mut store = dsh_frecency::FrecencyStore::default();
        store.add("initial_cmd", None);
        store.changed = true; // Simulate dirty state
        history.store = Some(Arc::new(store));

        // Create a "snapshot" by cloning the Arc (simulating background save start)
        let snapshot_arc = Arc::clone(history.store.as_ref().unwrap());

        // Modify history (simulating user typing immediately after save starts)
        history.add("new_cmd");

        // Force reset change flag on the new state (as save_background does via make_mut)
        if let Some(ref mut s) = history.store {
            let s_mut = Arc::make_mut(s);
            s_mut.changed = false;
        }

        // Verification:
        // 1. Snapshot should NOT have "new_cmd"
        // 2. Snapshot should still be marked as changed=true (from original state)
        // 3. Current history SHOULD have "new_cmd"
        // 4. Current history should be changed=false (we reset it)

        let snapshot = snapshot_arc.as_ref();
        let current = history.store.as_ref().unwrap().as_ref();

        // 1. Snapshot items check
        assert!(snapshot.items.iter().any(|i| i.item == "initial_cmd"));
        assert!(!snapshot.items.iter().any(|i| i.item == "new_cmd"));

        // 2. Snapshot dirty flag check
        assert!(
            snapshot.changed,
            "Snapshot should retain original dirty flag"
        );

        // 3. Current items check
        assert!(current.items.iter().any(|i| i.item == "initial_cmd"));
        assert!(current.items.iter().any(|i| i.item == "new_cmd"));

        // 4. Current dirty flag check
        assert!(
            !current.changed,
            "Current store should have dirty flag reset"
        );

        // 5. Pointer inequality check (proving they are distinct allocations now)
        assert!(
            !std::ptr::eq(snapshot, current),
            "Snapshot and current store should point to different memory locations"
        );

        Ok(())
    }

    #[test]
    fn test_async_history_update() {
        use parking_lot::Mutex;
        use std::sync::{Arc, Barrier};
        use std::thread;

        // Simulate the async loading pattern used in lib.rs
        let history = Arc::new(Mutex::new(FrecencyHistory::new()));
        let history_clone = history.clone();

        // Barrier to synchronize start
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();

        let handle = thread::spawn(move || {
            barrier_clone.wait();
            // Simulate heavy load
            let mut store = FrecencyStore::default();
            store.add("async_cmd", None);

            // "Load" complete - swap data
            let mut guard = history_clone.lock();
            guard.store = Some(Arc::new(store));
        });

        // Initially empty
        assert!(
            history.lock().store.is_none()
                || history.lock().store.as_ref().unwrap().items.is_empty()
        );

        barrier.wait();
        handle.join().unwrap();

        // Should now have data
        let guard = history.lock();
        assert!(guard.store.is_some());
        assert!(
            guard
                .store
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|i| i.item == "async_cmd")
        );
    }

    #[test]
    fn test_background_writer() -> Result<()> {
        init();
        let db_file = "dsh_test_background_writer.db";
        let db_path = environment::get_data_file(db_file)?;
        let _ = std::fs::remove_file(&db_path); // Cleanup

        // Create history and start background writer
        let mut history = History::from_file("dsh_test_background_writer")?;
        history.start_background_writer();

        // Write batch
        let entries = vec![
            ("cmd_async_1".to_string(), 1000),
            ("cmd_async_2".to_string(), 1001),
        ];
        history.write_batch(entries)?;

        // Wait a bit for background thread to process
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Verify with new connection
        let db = Db::new(db_path)?;
        let conn = db.get_connection();
        let mut stmt = conn
            .prepare("SELECT command, timestamp, count FROM command_history WHERE command = ?1")?;

        // Check cmd_async_1
        let row: (String, i64, i64) =
            stmt.query_row(["cmd_async_1"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        assert_eq!(row.0, "cmd_async_1");
        assert_eq!(row.1, 1000);
        assert_eq!(row.2, 1);

        // Check cmd_async_2
        let row2: (String, i64, i64) =
            stmt.query_row(["cmd_async_2"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        assert_eq!(row2.0, "cmd_async_2");
        assert_eq!(row2.1, 1001);

        Ok(())
    }
}
