//! Command history management.
//!
//! Provides the main command history storage with SQLite persistence,
//! background writing, and prefix-based search.

use super::context::get_current_context;
use super::entry::Entry;
use crate::db::Db;
use crate::environment;
use anyhow::Result;
use chrono::Local;
use std::sync::mpsc::{self, Sender};
use std::thread;

/// Message types for background history writer.
enum HistoryMsg {
    WriteBatch(Vec<(String, i64)>, Option<String>), // entries, context
    UpdateMetadata(String, Option<String>, HistoryMetadata),
}

#[derive(Debug, Clone, Default)]
pub struct HistoryMetadata {
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum HistoryScope {
    #[default]
    Global,
    Session,
    Cwd,
    Project,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum HistoryStatusFilter {
    #[default]
    Any,
    Success,
    Failure,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub text: Option<String>,
    pub scope: HistoryScope,
    pub status: HistoryStatusFilter,
    pub min_duration_ms: Option<u64>,
    pub limit: Option<usize>,
    pub current_cwd: Option<String>,
    pub current_project: Option<String>,
    pub current_session_id: Option<String>,
}

/// Command history with SQLite persistence.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct History {
    pub(crate) db: Option<Db>,
    pub(crate) histories: Vec<Entry>,
    size: usize,
    current_index: usize,
    pub search_word: Option<String>,
    sender: Option<Sender<HistoryMsg>>,
    /// Cache of recent entries for fast prefix search (max 100 entries)
    recent_cache: Vec<String>,
    /// Lowercase command text aligned with `histories` for allocation-free text search.
    normalized_entries: Vec<String>,
}

#[allow(dead_code)]
impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// Create a new empty history.
    pub fn new() -> Self {
        History {
            db: None,
            histories: Vec::new(),
            size: 10000,
            current_index: 0,
            search_word: None,
            sender: None,
            recent_cache: Vec::with_capacity(100),
            normalized_entries: Vec::new(),
        }
    }

    /// Create a history instance from a database file.
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
            recent_cache: Vec::with_capacity(100),
            normalized_entries: Vec::new(),
        })
    }

    fn normalized_command(command: &str) -> String {
        command.to_lowercase()
    }

    fn rebuild_normalized_entries(&mut self) {
        self.normalized_entries = self
            .histories
            .iter()
            .map(|entry| Self::normalized_command(&entry.entry))
            .collect();
    }

    fn get(&self, index: usize) -> Option<String> {
        if index < self.histories.len() {
            let entry = &self.histories[index].entry;
            Some(entry.to_string())
        } else {
            None
        }
    }

    /// Navigate backward through history.
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

    /// Navigate forward through history.
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

    /// Reset history index to the end.
    pub fn reset_index(&mut self) {
        self.current_index = self.histories.len();
    }

    /// Check if at the end of history.
    pub fn at_end(&self) -> bool {
        self.current_index == self.histories.len()
    }

    /// Check if at the latest entry.
    pub fn at_latest_entry(&self) -> bool {
        self.current_index == self.histories.len().saturating_sub(1)
    }

    /// Load all history entries.
    pub fn load(&mut self) -> Result<usize> {
        self.load_recent(10000).map(|_| self.histories.len())
    }

    /// Load recent history entries up to the given limit.
    pub fn load_recent(&mut self, limit: usize) -> Result<i64> {
        let mut min_timestamp = 0;
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history 
                    ORDER BY timestamp DESC 
                    LIMIT ?1
                 ) 
                 ORDER BY timestamp ASC",
            )?;

            let rows = stmt.query_map([limit as i64], |row| {
                Ok(Entry {
                    entry: row.get(0)?,
                    when: row.get(1)?,
                    count: row.get(2).unwrap_or(1),
                    context: row.get(3).ok(),
                    exit_code: row.get(4).ok(),
                    duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    cwd: row.get(6).ok(),
                    session_id: row.get(7).ok(),
                    hostname: row.get(8).ok(),
                })
            })?;

            self.histories.clear();

            for r in rows.flatten() {
                self.histories.push(r);
            }

            if let Some(first) = self.histories.first() {
                min_timestamp = first.when;
            }

            self.current_index = self.histories.len();

            // Initialize recent cache from loaded history (last 100 entries)
            self.recent_cache.clear();
            for entry in self.histories.iter().rev().take(100) {
                self.recent_cache.insert(0, entry.entry.clone());
            }
        }
        self.rebuild_normalized_entries();
        Ok(min_timestamp)
    }

    /// Load entries older than the given timestamp.
    pub fn load_older_than(&self, timestamp: i64, limit: usize) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history 
                    WHERE timestamp < ?1
                    ORDER BY timestamp DESC 
                    LIMIT ?2
                 ) 
                 ORDER BY timestamp ASC",
            )?;

            let rows = stmt.query_map(rusqlite::params![timestamp, limit as i64], |row| {
                Ok(Entry {
                    entry: row.get(0)?,
                    when: row.get(1)?,
                    count: row.get(2).unwrap_or(1),
                    context: row.get(3).ok(),
                    exit_code: row.get(4).ok(),
                    duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    cwd: row.get(6).ok(),
                    session_id: row.get(7).ok(),
                    hostname: row.get(8).ok(),
                })
            })?;

            for r in rows.flatten() {
                entries.push(r);
            }
        }
        Ok(entries)
    }

    /// Prepend entries to the beginning of history.
    pub fn prepend(&mut self, mut entries: Vec<Entry>) {
        entries.append(&mut self.histories);
        self.histories = entries;
        self.rebuild_normalized_entries();
        self.reset_index();
    }

    /// Reload history from the database.
    pub fn reload(&mut self) -> Result<()> {
        let db = if let Some(db) = &self.db {
            db.clone()
        } else {
            return Ok(());
        };

        // Only reload if we are not in the middle of navigation (at end of history)
        if !self.at_end() {
            return Ok(());
        }

        let conn = db.get_connection();
        let mut stmt = conn.prepare(
            "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
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
                context: row.get(3).ok(),
                exit_code: row.get(4).ok(),
                duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                cwd: row.get(6).ok(),
                session_id: row.get(7).ok(),
                hostname: row.get(8).ok(),
            })
        })?;

        let mut new_histories: Vec<Entry> = Vec::new();
        for r in rows.flatten() {
            new_histories.push(r);
        }

        // Merge local entries that are newer than DB
        if let Some(last_db_entry) = new_histories.last() {
            let last_db_ts = last_db_entry.when;

            for local_item in &self.histories {
                if local_item.when >= last_db_ts
                    && !new_histories.iter().any(|h| h.entry == local_item.entry)
                {
                    new_histories.push(Entry {
                        entry: local_item.entry.clone(),
                        when: local_item.when,
                        count: local_item.count,
                        context: local_item.context.clone(),
                        exit_code: local_item.exit_code,
                        duration_ms: local_item.duration_ms,
                        cwd: local_item.cwd.clone(),
                        session_id: local_item.session_id.clone(),
                        hostname: local_item.hostname.clone(),
                    });
                }
            }
        } else {
            // DB empty. Keep all local
            for local_item in &self.histories {
                new_histories.push(Entry {
                    entry: local_item.entry.clone(),
                    when: local_item.when,
                    count: local_item.count,
                    context: local_item.context.clone(),
                    exit_code: local_item.exit_code,
                    duration_ms: local_item.duration_ms,
                    cwd: local_item.cwd.clone(),
                    session_id: local_item.session_id.clone(),
                    hostname: local_item.hostname.clone(),
                });
            }
        }

        self.histories = new_histories;
        self.rebuild_normalized_entries();
        self.reset_index();

        // Update recent cache after reload
        self.recent_cache.clear();
        for entry in self.histories.iter().rev().take(100) {
            self.recent_cache.insert(0, entry.entry.clone());
        }

        Ok(())
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
                        HistoryMsg::WriteBatch(entries, context) => {
                            let _ = Self::write_batch_sync(&mut db, entries, context);
                        }
                        HistoryMsg::UpdateMetadata(command, context, metadata) => {
                            let _ =
                                Self::update_metadata_sync(&mut db, &command, context, &metadata);
                        }
                    }
                }
            });
        }
    }

    /// Synchronously write a batch of entries to the database.
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

    fn update_metadata_sync(
        db: &mut Db,
        command: &str,
        context: Option<String>,
        metadata: &HistoryMetadata,
    ) -> Result<()> {
        let conn = db.get_connection();
        conn.execute(
            "UPDATE command_history
             SET context = COALESCE(?2, context),
                 exit_code = ?3,
                 duration_ms = ?4,
                 cwd = ?5,
                 session_id = ?6,
                 hostname = ?7
             WHERE command = ?1",
            rusqlite::params![
                command,
                context,
                metadata.exit_code,
                metadata.duration_ms.map(|v| v as i64),
                metadata.cwd,
                metadata.session_id,
                metadata.hostname
            ],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn open(&mut self) -> Result<&mut History> {
        Ok(self)
    }

    #[allow(dead_code)]
    pub(crate) fn close(&mut self) -> Result<()> {
        Ok(())
    }

    /// Write a single history entry.
    pub fn write_history(&mut self, history: &str) -> Result<()> {
        self.write_batch(vec![(history.to_string(), Local::now().timestamp())])
    }

    /// Write a batch of history entries.
    pub fn write_batch(&mut self, entries: Vec<(String, i64)>) -> Result<()> {
        let context = get_current_context();

        if self.normalized_entries.len() != self.histories.len() {
            self.rebuild_normalized_entries();
        }

        // 1. Update in-memory history immediately
        for (cmd, when) in &entries {
            let mut count = 1;
            if let Some(pos) = self.histories.iter().position(|e| e.entry == *cmd) {
                count = self.histories[pos].count + 1;
                self.histories.remove(pos);
                self.normalized_entries.remove(pos);
            }
            self.histories.push(Entry {
                entry: cmd.clone(),
                when: *when,
                count,
                context: context.clone(),
                exit_code: None,
                duration_ms: None,
                cwd: None,
                session_id: None,
                hostname: None,
            });
            self.normalized_entries.push(Self::normalized_command(cmd));
        }
        self.reset_index();

        // Update recent cache
        for (cmd, _) in &entries {
            self.recent_cache.retain(|e| e != cmd);
            self.recent_cache.push(cmd.clone());
            if self.recent_cache.len() > 100 {
                self.recent_cache.remove(0);
            }
        }

        // 2. Persist
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::WriteBatch(entries, context));
        } else if let Some(db) = &mut self.db {
            let _ = Self::write_batch_sync(db, entries, context);
        }
        Ok(())
    }

    pub fn record_outcome(&mut self, command: &str, metadata: HistoryMetadata) -> Result<()> {
        if let Some(entry) = self
            .histories
            .iter_mut()
            .rev()
            .find(|entry| entry.entry == command)
        {
            entry.context = get_current_context();
            entry.exit_code = metadata.exit_code;
            entry.duration_ms = metadata.duration_ms;
            entry.cwd = metadata.cwd.clone();
            entry.session_id = metadata.session_id.clone();
            entry.hostname = metadata.hostname.clone();
        }

        let context = get_current_context();
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::UpdateMetadata(
                command.to_string(),
                context,
                metadata,
            ));
        } else if let Some(db) = &mut self.db {
            let _ = Self::update_metadata_sync(db, command, context, &metadata);
        }
        Ok(())
    }

    /// Search for the first entry matching the given prefix.
    pub fn search_first(&self, word: &str) -> Option<&str> {
        // First, check recent cache (fast path)
        for entry in self.recent_cache.iter().rev() {
            if entry.starts_with(word) {
                return Some(entry);
            }
        }
        // Fall back to full history search
        for hist in self.histories.iter().rev() {
            if hist.entry.starts_with(word) {
                return Some(&hist.entry);
            }
        }
        None
    }

    /// Get recent commands for context.
    pub fn get_recent_context(&self, limit: usize) -> Vec<String> {
        self.histories
            .iter()
            .rev()
            .take(limit)
            .map(|e| e.entry.clone())
            .collect()
    }

    pub fn search_entries(&self, query: &HistoryQuery) -> Vec<Entry> {
        if query.limit == Some(0) {
            return Vec::new();
        }

        let normalized_text = query.text.as_ref().map(|text| text.to_lowercase());
        let cache_usable = self.normalized_entries.len() == self.histories.len();
        let mut matched = Vec::new();

        for (index, entry) in self.histories.iter().enumerate().rev() {
            if let Some(text) = &normalized_text {
                let contains_text = if cache_usable {
                    self.normalized_entries[index].contains(text)
                } else {
                    entry.entry.to_lowercase().contains(text)
                };
                if !contains_text {
                    continue;
                }
            }

            match query.status {
                HistoryStatusFilter::Any => {}
                HistoryStatusFilter::Success => {
                    if entry.exit_code != Some(0) {
                        continue;
                    }
                }
                HistoryStatusFilter::Failure => {
                    if entry.exit_code.is_none() || entry.exit_code == Some(0) {
                        continue;
                    }
                }
            }

            if let Some(min_duration_ms) = query.min_duration_ms
                && entry.duration_ms.unwrap_or_default() < min_duration_ms
            {
                continue;
            }

            match query.scope {
                HistoryScope::Global => {}
                HistoryScope::Session => {
                    if entry.session_id.as_deref() != query.current_session_id.as_deref() {
                        continue;
                    }
                }
                HistoryScope::Cwd => {
                    if entry.cwd.as_deref() != query.current_cwd.as_deref() {
                        continue;
                    }
                }
                HistoryScope::Project => {
                    if entry.context.as_deref() != query.current_project.as_deref() {
                        continue;
                    }
                }
            }

            matched.push(entry.clone());
            if let Some(limit) = query.limit
                && matched.len() >= limit
            {
                break;
            }
        }

        matched
    }

    /// Get an iterator over history entries.
    pub fn iter(&self) -> std::slice::Iter<'_, Entry> {
        self.histories.iter()
    }

    /// Add a test entry (for testing only).
    #[cfg(test)]
    pub fn add_test_entry(&mut self, entry: &str) {
        self.histories.push(Entry {
            entry: entry.to_string(),
            when: Local::now().timestamp(),
            count: 1,
            context: None,
            exit_code: None,
            duration_ms: None,
            cwd: None,
            session_id: None,
            hostname: None,
        });
        self.normalized_entries
            .push(Self::normalized_command(entry));
        self.size = self.histories.len();
        self.current_index = self.histories.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(
        entry: &str,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
        cwd: Option<&str>,
        context: Option<&str>,
        session_id: Option<&str>,
    ) -> Entry {
        Entry {
            entry: entry.to_string(),
            when: Local::now().timestamp(),
            count: 1,
            context: context.map(str::to_string),
            exit_code,
            duration_ms,
            cwd: cwd.map(str::to_string),
            session_id: session_id.map(str::to_string),
            hostname: Some("test-host".to_string()),
        }
    }

    #[test]
    fn search_entries_filters_by_scope_status_and_query() {
        let mut history = History::new();
        history.histories = vec![
            sample_entry(
                "cargo test",
                Some(0),
                Some(1200),
                Some("/repo"),
                Some("/repo"),
                Some("session-a"),
            ),
            sample_entry(
                "cargo build",
                Some(1),
                Some(3200),
                Some("/repo"),
                Some("/repo"),
                Some("session-a"),
            ),
            sample_entry(
                "npm test",
                Some(0),
                Some(800),
                Some("/web"),
                Some("/web"),
                Some("session-b"),
            ),
        ];

        let query = HistoryQuery {
            text: Some("cargo".to_string()),
            scope: HistoryScope::Session,
            status: HistoryStatusFilter::Failure,
            min_duration_ms: Some(1000),
            limit: None,
            current_cwd: Some("/repo".to_string()),
            current_project: Some("/repo".to_string()),
            current_session_id: Some("session-a".to_string()),
        };

        let results = history.search_entries(&query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry, "cargo build");
    }

    #[test]
    fn search_entries_uses_recent_order_and_limit() {
        let mut history = History::new();
        history
            .write_batch(vec![
                ("Git Status".to_string(), 1),
                ("git commit".to_string(), 2),
                ("cargo test".to_string(), 3),
                ("git checkout main".to_string(), 4),
            ])
            .unwrap();

        let query = HistoryQuery {
            text: Some("GIT".to_string()),
            limit: Some(2),
            ..Default::default()
        };

        let results = history.search_entries(&query);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry, "git checkout main");
        assert_eq!(results[1].entry, "git commit");
    }
}
