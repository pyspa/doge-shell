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
use parking_lot::Mutex as ParkingMutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

const LEDGER_RETENTION_SECONDS: i64 = 90 * 24 * 60 * 60;
const LEDGER_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const LEDGER_MAX_EVENTS: i64 = 10_000;

fn truncate_ledger_output(output: &str) -> String {
    if output.len() <= LEDGER_MAX_OUTPUT_BYTES {
        return output.to_string();
    }
    let mut end = LEDGER_MAX_OUTPUT_BYTES;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated)", &output[..end])
}

fn enqueue_atuin_dual_write(event: CommandEvent) {
    let enabled = std::env::var("DSH_ATUIN_DUAL_WRITE").ok().as_deref() == Some("1");
    enqueue_atuin_dual_write_with(enabled, std::path::PathBuf::from("atuin"), event);
}

fn enqueue_atuin_dual_write_with(
    enabled: bool,
    executable: std::path::PathBuf,
    event: CommandEvent,
) {
    if !enabled {
        return;
    }
    thread::spawn(move || {
        use std::process::{Command, Stdio};
        use std::time::Duration;
        use wait_timeout::ChildExt;

        let end_executable = executable.clone();
        let mut start = Command::new(executable);
        start
            .args(["history", "start", "--", event.command.as_str()])
            .env("ATUIN_HISTORY_AUTHOR", event.author.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(cwd) = event.cwd.as_deref() {
            start.current_dir(cwd);
        }
        if let Some(session) = event.session_id.as_deref() {
            start.env("ATUIN_SESSION", session);
        }
        let Ok(mut child) = start.spawn() else {
            return;
        };
        let completed = child
            .wait_timeout(Duration::from_millis(750))
            .ok()
            .flatten();
        if completed.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        let Ok(output) = child.wait_with_output() else {
            return;
        };
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if id.is_empty() {
            return;
        }
        let exit = event.exit_code.unwrap_or_default().to_string();
        let Ok(mut end) = Command::new(end_executable)
            .args(["history", "end", "--exit", exit.as_str(), "--", id.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        if end
            .wait_timeout(Duration::from_millis(750))
            .ok()
            .flatten()
            .is_none()
        {
            let _ = end.kill();
            let _ = end.wait();
        }
    });
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum CommandLedgerMode {
    #[default]
    Off,
    Metadata,
    Output,
}

impl CommandLedgerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Metadata => "metadata",
            Self::Output => "output",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "metadata" => Some(Self::Metadata),
            "output" => Some(Self::Output),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandEvent {
    #[serde(default)]
    pub id: i64,
    pub command: String,
    pub cwd: Option<String>,
    pub started_at: i64,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    #[serde(rename = "session", alias = "session_id")]
    pub session_id: Option<String>,
    #[serde(rename = "host", alias = "hostname")]
    pub hostname: Option<String>,
    pub author: String,
    pub output: Option<String>,
}

/// Message types for background history writer.
enum HistoryMsg {
    WriteBatch {
        entries: Vec<(String, i64)>,
        context: Option<String>,
        persistence_id: u64,
        commands: Vec<String>,
    },
    RecordOutcome {
        command: String,
        context: Option<String>,
        metadata: HistoryMetadata,
        persistence_id: u64,
    },
    Reload {
        base_revision: u64,
        pending_entries: Vec<Entry>,
        complete: CommandHistoryReloadCallback,
    },
}

#[derive(Debug)]
struct HistoryPersistAck {
    persistence_id: u64,
    commands: Vec<String>,
    result: Result<(), String>,
}

#[derive(Debug, Clone)]
struct PendingHistoryEntry {
    persistence_id: u64,
    entry: Entry,
}

type CommandHistoryReloadCallback =
    Box<dyn FnOnce(Result<CommandHistoryReloadSnapshot>) + Send + 'static>;

#[derive(Debug)]
pub(crate) struct CommandHistoryReloadSnapshot {
    base_revision: u64,
    histories: Vec<Entry>,
    normalized_entries: Vec<String>,
    recent_cache: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryReloadApply {
    Applied,
    Navigating,
    Stale,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryMetadata {
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub hostname: Option<String>,
    pub started_at: i64,
    pub author: String,
    pub output: Option<String>,
    pub ledger_mode: CommandLedgerMode,
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

/// Applies a [`HistoryQuery`]'s filters to individual entries.
///
/// Holds the lowercased query text so a scan does not redo that work per entry.
/// Shared by [`History::search_entries`] and the interactive Ctrl-R picker so
/// the two cannot drift apart; the result limit is the caller's business.
pub struct EntryMatcher<'q> {
    query: &'q HistoryQuery,
    normalized_text: Option<String>,
}

impl<'q> EntryMatcher<'q> {
    pub fn new(query: &'q HistoryQuery) -> Self {
        Self {
            normalized_text: query.text.as_ref().map(|text| text.to_lowercase()),
            query,
        }
    }

    /// `normalized_entry` is the pre-lowercased command text when the caller
    /// maintains a cache for it; `None` lowercases on the fly.
    pub fn matches(&self, entry: &Entry, normalized_entry: Option<&str>) -> bool {
        if let Some(text) = &self.normalized_text {
            let contains_text = match normalized_entry {
                Some(normalized) => normalized.contains(text.as_str()),
                None => entry.entry.to_lowercase().contains(text.as_str()),
            };
            if !contains_text {
                return false;
            }
        }

        match self.query.status {
            HistoryStatusFilter::Any => {}
            HistoryStatusFilter::Success => {
                if entry.exit_code != Some(0) {
                    return false;
                }
            }
            HistoryStatusFilter::Failure => {
                if entry.exit_code.is_none() || entry.exit_code == Some(0) {
                    return false;
                }
            }
        }

        if let Some(min_duration_ms) = self.query.min_duration_ms
            && entry.duration_ms.unwrap_or_default() < min_duration_ms
        {
            return false;
        }

        match self.query.scope {
            HistoryScope::Global => {}
            HistoryScope::Session => {
                if entry.session_id.as_deref() != self.query.current_session_id.as_deref() {
                    return false;
                }
            }
            HistoryScope::Cwd => {
                if entry.cwd.as_deref() != self.query.current_cwd.as_deref() {
                    return false;
                }
            }
            HistoryScope::Project => {
                if entry.context.as_deref() != self.query.current_project.as_deref() {
                    return false;
                }
            }
        }

        true
    }
}

/// Command history with SQLite persistence.
#[derive(Debug, Clone)]
pub struct History {
    pub(crate) db: Option<Db>,
    pub(crate) histories: Vec<Entry>,
    // Retained for potential capacity bookkeeping; not read yet.
    #[allow(dead_code)]
    size: usize,
    current_index: usize,
    pub search_word: Option<String>,
    sender: Option<Sender<HistoryMsg>>,
    /// Cache of recent entries for fast prefix search (max 100 entries)
    recent_cache: Vec<String>,
    /// Lowercase command text aligned with `histories` for allocation-free text search.
    normalized_entries: Vec<String>,
    /// Incremented whenever the in-memory history changes. Background reloads
    /// only replace the snapshot they started from, so local writes cannot be
    /// overwritten by an older database read.
    revision: u64,
    /// Entries whose most recent SQLite write has not been acknowledged yet.
    /// Only this small delta is copied into a background reload request.
    pending_persistence: HashMap<String, PendingHistoryEntry>,
    persist_ack_rx: Option<Arc<ParkingMutex<Receiver<HistoryPersistAck>>>>,
    next_persistence_id: u64,
}

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
            revision: 0,
            pending_persistence: HashMap::new(),
            persist_ack_rx: None,
            next_persistence_id: 0,
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
            revision: 0,
            pending_persistence: HashMap::new(),
            persist_ack_rx: None,
            next_persistence_id: 0,
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

    fn search_is_case_sensitive(word: &str) -> bool {
        word.chars().any(|ch| ch.is_uppercase())
    }

    fn find_previous_match(&mut self, start: usize, word: &str) -> Option<(usize, String)> {
        let case_sensitive = Self::search_is_case_sensitive(word);
        let normalized_word = if case_sensitive {
            None
        } else {
            if self.normalized_entries.len() != self.histories.len() {
                self.rebuild_normalized_entries();
            }
            Some(Self::normalized_command(word))
        };
        let needle = normalized_word.as_deref().unwrap_or(word);

        for index in (0..=start).rev() {
            let haystack = if case_sensitive {
                self.histories[index].entry.as_str()
            } else {
                self.normalized_entries[index].as_str()
            };
            if haystack.contains(needle) {
                return Some((index, self.histories[index].entry.clone()));
            }
        }

        None
    }

    fn find_next_match(&mut self, start: usize, word: &str) -> Option<(usize, String)> {
        let case_sensitive = Self::search_is_case_sensitive(word);
        let normalized_word = if case_sensitive {
            None
        } else {
            if self.normalized_entries.len() != self.histories.len() {
                self.rebuild_normalized_entries();
            }
            Some(Self::normalized_command(word))
        };
        let needle = normalized_word.as_deref().unwrap_or(word);

        for index in start..self.histories.len() {
            let haystack = if case_sensitive {
                self.histories[index].entry.as_str()
            } else {
                self.normalized_entries[index].as_str()
            };
            if haystack.contains(needle) {
                return Some((index, self.histories[index].entry.clone()));
            }
        }

        None
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
        if self.current_index == 0 {
            return None;
        }

        let start = self.current_index - 1;
        match self.search_word.clone() {
            Some(word) => {
                if let Some((index, entry)) = self.find_previous_match(start, &word) {
                    self.current_index = index;
                    Some(entry)
                } else {
                    None
                }
            }
            None => {
                self.current_index = start;
                self.get(self.current_index)
            }
        }
    }

    /// Navigate forward through history.
    pub fn forward(&mut self) -> Option<String> {
        let start = self.current_index + 1;
        if start >= self.histories.len() {
            if self.search_word.is_some() {
                self.reset_index();
            }
            return None;
        }

        match self.search_word.clone() {
            Some(word) => {
                if let Some((index, entry)) = self.find_next_match(start, &word) {
                    self.current_index = index;
                    Some(entry)
                } else {
                    self.reset_index();
                    None
                }
            }
            None => {
                self.current_index = start;
                self.get(self.current_index)
            }
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
        self.bump_revision();
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
        self.bump_revision();
    }

    /// Reload history from the database.
    pub fn reload(&mut self) -> Result<()> {
        let Some(db) = self.db.clone() else {
            return Ok(());
        };

        // Only reload if we are not in the middle of navigation (at end of history)
        if !self.at_end() {
            return Ok(());
        }

        self.drain_persistence_acks();
        let snapshot =
            Self::load_reload_snapshot(&db, self.revision, self.pending_entries_snapshot())?;
        let _ = self.apply_reload_snapshot(snapshot);
        Ok(())
    }

    pub(crate) fn request_reload<F>(&mut self, complete: F) -> bool
    where
        F: FnOnce(Result<CommandHistoryReloadSnapshot>) + Send + 'static,
    {
        if !self.at_end() {
            return false;
        }
        self.drain_persistence_acks();
        let Some(sender) = &self.sender else {
            return false;
        };
        let pending_entries = self.pending_entries_snapshot();
        sender
            .send(HistoryMsg::Reload {
                base_revision: self.revision,
                pending_entries,
                complete: Box::new(complete),
            })
            .is_ok()
    }

    fn load_reload_snapshot(
        db: &Db,
        base_revision: u64,
        pending_entries: Vec<Entry>,
    ) -> Result<CommandHistoryReloadSnapshot> {
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

        let mut histories = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Self::merge_pending_entries(&mut histories, pending_entries);
        let normalized_entries = histories
            .iter()
            .map(|entry| Self::normalized_command(&entry.entry))
            .collect();
        let mut recent_cache = histories
            .iter()
            .rev()
            .take(100)
            .map(|entry| entry.entry.clone())
            .collect::<Vec<_>>();
        recent_cache.reverse();

        Ok(CommandHistoryReloadSnapshot {
            base_revision,
            histories,
            normalized_entries,
            recent_cache,
        })
    }

    fn merge_pending_entries(histories: &mut Vec<Entry>, pending_entries: Vec<Entry>) {
        for local in pending_entries {
            match histories
                .iter()
                .position(|entry| entry.entry == local.entry)
            {
                Some(index) if histories[index].when <= local.when => {
                    let db_entry = &mut histories[index];
                    db_entry.when = local.when;
                    db_entry.count = db_entry.count.max(local.count);
                    db_entry.context = local.context.or_else(|| db_entry.context.clone());
                    db_entry.exit_code = local.exit_code.or(db_entry.exit_code);
                    db_entry.duration_ms = local.duration_ms.or(db_entry.duration_ms);
                    db_entry.cwd = local.cwd.or_else(|| db_entry.cwd.clone());
                    db_entry.session_id = local.session_id.or_else(|| db_entry.session_id.clone());
                    db_entry.hostname = local.hostname.or_else(|| db_entry.hostname.clone());
                }
                Some(_) => {}
                None => histories.push(local),
            }
        }
        histories.sort_by(|left, right| {
            left.when
                .cmp(&right.when)
                .then(left.entry.cmp(&right.entry))
        });
    }

    fn next_persistence_id(&mut self) -> u64 {
        self.next_persistence_id = self.next_persistence_id.wrapping_add(1);
        self.next_persistence_id
    }

    fn track_pending_entries(&mut self, persistence_id: u64, commands: &[String]) {
        for command in commands {
            if let Some(entry) = self
                .histories
                .iter()
                .rev()
                .find(|entry| entry.entry == *command)
                .cloned()
            {
                self.pending_persistence.insert(
                    command.clone(),
                    PendingHistoryEntry {
                        persistence_id,
                        entry,
                    },
                );
            }
        }
    }

    fn acknowledge_persistence(&mut self, persistence_id: u64, commands: &[String]) {
        for command in commands {
            if self
                .pending_persistence
                .get(command)
                .is_some_and(|pending| pending.persistence_id == persistence_id)
            {
                self.pending_persistence.remove(command);
            }
        }
    }

    fn drain_persistence_acks(&mut self) {
        let acks = self
            .persist_ack_rx
            .as_ref()
            .map(|receiver| receiver.lock().try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for ack in acks {
            match ack.result {
                Ok(()) => self.acknowledge_persistence(ack.persistence_id, &ack.commands),
                Err(error) => {
                    tracing::warn!("background command history write failed: {error}");
                }
            }
        }
    }

    fn pending_entries_snapshot(&self) -> Vec<Entry> {
        self.pending_persistence
            .values()
            .map(|pending| pending.entry.clone())
            .collect()
    }

    pub(crate) fn apply_reload_snapshot(
        &mut self,
        snapshot: CommandHistoryReloadSnapshot,
    ) -> HistoryReloadApply {
        if !self.at_end() {
            return HistoryReloadApply::Navigating;
        }
        if self.revision != snapshot.base_revision {
            return HistoryReloadApply::Stale;
        }

        self.histories = snapshot.histories;
        self.normalized_entries = snapshot.normalized_entries;
        self.recent_cache = snapshot.recent_cache;
        self.reset_index();
        self.bump_revision();
        HistoryReloadApply::Applied
    }

    pub(crate) fn reload_snapshot_for_probe(
        &self,
        target: &History,
    ) -> CommandHistoryReloadSnapshot {
        CommandHistoryReloadSnapshot {
            base_revision: target.revision,
            histories: self.histories.clone(),
            normalized_entries: self.normalized_entries.clone(),
            recent_cache: self.recent_cache.clone(),
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Start the background writer thread.
    pub fn start_background_writer(&mut self) {
        if let Some(db_clone) = self.db.clone() {
            let (tx, rx) = mpsc::channel();
            let (ack_tx, ack_rx) = mpsc::channel();
            self.sender = Some(tx);
            self.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));

            thread::spawn(move || {
                let mut db = db_clone;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        HistoryMsg::WriteBatch {
                            entries,
                            context,
                            persistence_id,
                            commands,
                        } => {
                            let result = Self::write_batch_sync(&mut db, entries, context)
                                .map_err(|error| error.to_string());
                            let _ = ack_tx.send(HistoryPersistAck {
                                persistence_id,
                                commands,
                                result,
                            });
                        }
                        HistoryMsg::RecordOutcome {
                            command,
                            context,
                            metadata,
                            persistence_id,
                        } => {
                            let result =
                                Self::record_outcome_sync(&mut db, &command, context, &metadata)
                                    .map_err(|error| error.to_string());
                            let _ = ack_tx.send(HistoryPersistAck {
                                persistence_id,
                                commands: vec![command],
                                result,
                            });
                        }
                        HistoryMsg::Reload {
                            base_revision,
                            pending_entries,
                            complete,
                        } => {
                            complete(Self::load_reload_snapshot(
                                &db,
                                base_revision,
                                pending_entries,
                            ));
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

    fn record_outcome_sync(
        db: &mut Db,
        command: &str,
        context: Option<String>,
        metadata: &HistoryMetadata,
    ) -> Result<()> {
        Self::update_metadata_sync(db, command, context, metadata)?;
        if metadata.ledger_mode == CommandLedgerMode::Off {
            return Ok(());
        }
        let output = (metadata.ledger_mode == CommandLedgerMode::Output)
            .then(|| metadata.output.as_deref().map(truncate_ledger_output))
            .flatten();
        let conn = db.get_connection();
        conn.execute(
            "INSERT INTO command_events
             (command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                command,
                metadata.cwd,
                metadata.started_at,
                metadata.duration_ms.map(|value| value as i64),
                metadata.exit_code,
                metadata.session_id,
                metadata.hostname,
                metadata.author,
                output,
            ],
        )?;
        conn.execute(
            "DELETE FROM command_events WHERE started_at < ?1",
            [chrono::Utc::now().timestamp() - LEDGER_RETENTION_SECONDS],
        )?;
        conn.execute(
            "DELETE FROM command_events
             WHERE id NOT IN (
                 SELECT id FROM command_events
                 ORDER BY started_at DESC, id DESC LIMIT ?1
             )",
            [LEDGER_MAX_EVENTS],
        )?;
        enqueue_atuin_dual_write(CommandEvent {
            id: 0,
            command: command.to_string(),
            cwd: metadata.cwd.clone(),
            started_at: metadata.started_at,
            duration_ms: metadata.duration_ms,
            exit_code: metadata.exit_code,
            session_id: metadata.session_id.clone(),
            hostname: metadata.hostname.clone(),
            author: metadata.author.clone(),
            output: None,
        });
        Ok(())
    }

    // Lifecycle hooks kept for API symmetry with backing stores that
    // require explicit open/close; currently no-ops.
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
        self.drain_persistence_acks();
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
        if !entries.is_empty() {
            self.bump_revision();
        }

        // 2. Persist
        let persistence_id = self.next_persistence_id();
        let commands = entries
            .iter()
            .map(|(command, _)| command.clone())
            .collect::<Vec<_>>();
        self.track_pending_entries(persistence_id, &commands);
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::WriteBatch {
                entries,
                context,
                persistence_id,
                commands,
            });
        } else if let Some(db) = &mut self.db
            && Self::write_batch_sync(db, entries, context).is_ok()
        {
            self.acknowledge_persistence(persistence_id, &commands);
        }
        Ok(())
    }

    pub fn record_outcome(&mut self, command: &str, metadata: HistoryMetadata) -> Result<()> {
        self.drain_persistence_acks();
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
        let persistence_id = self.next_persistence_id();
        let commands = vec![command.to_string()];
        self.track_pending_entries(persistence_id, &commands);
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::RecordOutcome {
                command: command.to_string(),
                context,
                metadata,
                persistence_id,
            });
        } else if let Some(db) = &mut self.db
            && Self::record_outcome_sync(db, command, context, &metadata).is_ok()
        {
            self.acknowledge_persistence(persistence_id, &commands);
        }
        self.bump_revision();
        Ok(())
    }

    pub fn command_events(&self, author: Option<&str>, limit: usize) -> Result<Vec<CommandEvent>> {
        self.command_events_filtered(author, limit, false)
    }

    pub fn command_events_filtered(
        &self,
        author: Option<&str>,
        limit: usize,
        failures_only: bool,
    ) -> Result<Vec<CommandEvent>> {
        let Some(db) = &self.db else {
            return Ok(Vec::new());
        };
        let conn = db.get_connection();
        let author = author.filter(|author| *author != "all");
        let sql = match (author.is_some(), failures_only) {
            (true, true) => {
                "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output
                 FROM command_events WHERE author = ?1 AND exit_code IS NOT NULL AND exit_code != 0
                 ORDER BY started_at DESC, id DESC LIMIT ?2"
            }
            (true, false) => {
                "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output
                 FROM command_events WHERE author = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2"
            }
            (false, true) => {
                "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output
                 FROM command_events WHERE exit_code IS NOT NULL AND exit_code != 0
                 ORDER BY started_at DESC, id DESC LIMIT ?1"
            }
            (false, false) => {
                "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output
                 FROM command_events ORDER BY started_at DESC, id DESC LIMIT ?1"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(CommandEvent {
                id: row.get(0)?,
                command: row.get(1)?,
                cwd: row.get(2)?,
                started_at: row.get(3)?,
                duration_ms: row
                    .get::<_, Option<i64>>(4)?
                    .map(|value| value.max(0) as u64),
                exit_code: row.get(5)?,
                session_id: row.get(6)?,
                hostname: row.get(7)?,
                author: row.get(8)?,
                output: row.get(9)?,
            })
        };
        let rows = if let Some(author) = author {
            stmt.query_map(rusqlite::params![author, limit as i64], map_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map(rusqlite::params![limit as i64], map_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    pub fn record_external_event(&mut self, event: CommandEvent) -> Result<()> {
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        let metadata = HistoryMetadata {
            exit_code: event.exit_code,
            duration_ms: event.duration_ms,
            cwd: event.cwd,
            session_id: event.session_id,
            hostname: event.hostname,
            started_at: event.started_at,
            author: event.author,
            output: event.output,
            ledger_mode: CommandLedgerMode::Output,
        };
        Self::record_outcome_sync(db, &event.command, None, &metadata)
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

        let matcher = EntryMatcher::new(query);
        let cache_usable = self.normalized_entries.len() == self.histories.len();
        let mut matched = Vec::new();

        for (index, entry) in self.histories.iter().enumerate().rev() {
            let normalized_entry = cache_usable.then(|| self.normalized_entries[index].as_str());
            if !matcher.matches(entry, normalized_entry) {
                continue;
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

    /// The most recent `max` entries, newest first, as an owned snapshot.
    ///
    /// Lets an interactive picker re-filter on every keystroke without holding
    /// the history lock for the duration of the session.
    pub fn snapshot_entries(&self, max: usize) -> Vec<Entry> {
        self.histories.iter().rev().take(max).cloned().collect()
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
        self.bump_revision();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_metadata(author: &str, mode: CommandLedgerMode) -> HistoryMetadata {
        HistoryMetadata {
            exit_code: Some(1),
            duration_ms: Some(42),
            cwd: Some("/repo".to_string()),
            session_id: Some("session".to_string()),
            hostname: Some("host".to_string()),
            started_at: chrono::Utc::now().timestamp(),
            author: author.to_string(),
            output: Some("API_KEY=secret".to_string()),
            ledger_mode: mode,
        }
    }

    #[test]
    fn ledger_is_append_only_and_filters_by_author() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::new();
        history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
        history.write_history("cargo test").unwrap();
        history
            .record_outcome(
                "cargo test",
                ledger_metadata("human", CommandLedgerMode::Metadata),
            )
            .unwrap();
        history
            .record_outcome(
                "cargo test",
                ledger_metadata("agent-x", CommandLedgerMode::Metadata),
            )
            .unwrap();

        let all = history.command_events(Some("all"), 10).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|event| event.output.is_none()));
        let agent = history.command_events(Some("agent-x"), 10).unwrap();
        assert_eq!(agent.len(), 1);
        assert_eq!(agent[0].author, "agent-x");
    }

    #[test]
    fn output_mode_truncates_on_a_character_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::new();
        history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
        history.write_history("command").unwrap();
        let mut metadata = ledger_metadata("human", CommandLedgerMode::Output);
        metadata.output = Some("あ".repeat(LEDGER_MAX_OUTPUT_BYTES));
        history.record_outcome("command", metadata).unwrap();
        let event = history.command_events(Some("all"), 1).unwrap().remove(0);
        assert!(event.output.unwrap().ends_with("... (truncated)"));
    }

    #[test]
    fn recording_an_event_prunes_entries_outside_retention_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::new();
        history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
        {
            let conn = history.db.as_ref().unwrap().get_connection();
            conn.execute(
                "INSERT INTO command_events(command, started_at, author) VALUES ('old', 0, 'human')",
                [],
            )
            .unwrap();
        }
        history.write_history("new").unwrap();
        history
            .record_outcome("new", ledger_metadata("human", CommandLedgerMode::Metadata))
            .unwrap();
        let events = history.command_events(Some("all"), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].command, "new");
    }

    #[test]
    fn failure_filter_is_applied_before_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::new();
        history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
        let conn = history.db.as_ref().unwrap().get_connection();
        conn.execute(
            "INSERT INTO command_events(command, started_at, exit_code, author)
             VALUES ('old failure', 1, 1, 'human')",
            [],
        )
        .unwrap();
        for timestamp in 2..=102 {
            conn.execute(
                "INSERT INTO command_events(command, started_at, exit_code, author)
                 VALUES (?1, ?2, 0, 'human')",
                rusqlite::params![format!("success {timestamp}"), timestamp],
            )
            .unwrap();
        }
        drop(conn);

        let events = history
            .command_events_filtered(Some("all"), 1, true)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].command, "old failure");
    }

    #[test]
    fn disabled_or_broken_atuin_adapter_never_waits_for_command_execution() {
        let event = CommandEvent {
            id: 0,
            command: "cargo test".to_string(),
            cwd: None,
            started_at: 0,
            duration_ms: None,
            exit_code: None,
            session_id: None,
            hostname: None,
            author: "human".to_string(),
            output: None,
        };
        let start = std::time::Instant::now();
        enqueue_atuin_dual_write_with(
            true,
            std::path::PathBuf::from("/definitely/missing/atuin"),
            event,
        );
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn reload_snapshot_imports_another_sessions_command() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
        let mut other_session = History::new();
        other_session.db = Some(db.clone());
        other_session
            .write_batch(vec![("cargo test".to_string(), 100)])
            .unwrap();

        let mut history = History::new();
        history.db = Some(db.clone());
        let snapshot = History::load_reload_snapshot(&db, history.revision, Vec::new()).unwrap();

        assert_eq!(
            history.apply_reload_snapshot(snapshot),
            HistoryReloadApply::Applied
        );
        assert_eq!(history.iter().next().unwrap().entry, "cargo test");
    }

    #[test]
    fn stale_reload_snapshot_does_not_overwrite_local_append() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
        let mut history = History::new();
        history.db = Some(db.clone());
        let snapshot = History::load_reload_snapshot(&db, history.revision, Vec::new()).unwrap();

        history.add_test_entry("local while loading");

        assert_eq!(
            history.apply_reload_snapshot(snapshot),
            HistoryReloadApply::Stale
        );
        assert_eq!(history.iter().next().unwrap().entry, "local while loading");
    }

    #[test]
    fn reload_snapshot_is_discarded_during_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
        let mut history = History::new();
        history.db = Some(db.clone());
        history
            .write_batch(vec![("git status".to_string(), 100)])
            .unwrap();
        let snapshot = History::load_reload_snapshot(&db, history.revision, Vec::new()).unwrap();
        assert_eq!(history.back().as_deref(), Some("git status"));

        assert_eq!(
            history.apply_reload_snapshot(snapshot),
            HistoryReloadApply::Navigating
        );
        assert_eq!(history.iter().next().unwrap().entry, "git status");
    }

    #[test]
    fn reload_snapshot_keeps_an_unpersisted_local_entry() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
        let local = Entry {
            entry: "local after failed write".to_string(),
            when: 200,
            count: 1,
            context: Some("/repo".to_string()),
            exit_code: Some(1),
            duration_ms: Some(42),
            cwd: Some("/repo".to_string()),
            session_id: Some("local-session".to_string()),
            hostname: Some("local-host".to_string()),
        };

        let snapshot = History::load_reload_snapshot(&db, 0, vec![local.clone()]).unwrap();

        assert_eq!(snapshot.histories.len(), 1);
        let restored = &snapshot.histories[0];
        assert_eq!(restored.entry, local.entry);
        assert_eq!(restored.exit_code, local.exit_code);
        assert_eq!(restored.session_id, local.session_id);
    }

    #[test]
    fn failed_persistence_ack_keeps_the_local_reload_delta() {
        let mut history = History::new();
        history
            .write_batch(vec![("keep after sqlite failure".to_string(), 300)])
            .unwrap();
        let pending = history
            .pending_persistence
            .get("keep after sqlite failure")
            .unwrap();
        let persistence_id = pending.persistence_id;
        let (ack_tx, ack_rx) = mpsc::channel();
        history.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));
        ack_tx
            .send(HistoryPersistAck {
                persistence_id,
                commands: vec!["keep after sqlite failure".to_string()],
                result: Err("disk full".to_string()),
            })
            .unwrap();

        history.drain_persistence_acks();

        let pending = history.pending_entries_snapshot();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].entry, "keep after sqlite failure");
    }

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
    fn entry_matches_applies_scope() {
        let entry = sample_entry(
            "cargo build",
            Some(0),
            Some(100),
            Some("/repo/src"),
            Some("/repo"),
            Some("session-a"),
        );
        let context = HistoryQuery {
            current_cwd: Some("/repo/src".to_string()),
            current_project: Some("/repo".to_string()),
            current_session_id: Some("session-a".to_string()),
            ..Default::default()
        };

        for scope in [
            HistoryScope::Global,
            HistoryScope::Session,
            HistoryScope::Cwd,
            HistoryScope::Project,
        ] {
            let query = HistoryQuery {
                scope,
                ..context.clone()
            };
            assert!(
                EntryMatcher::new(&query).matches(&entry, None),
                "{scope:?} should match its own context"
            );
        }

        let elsewhere = HistoryQuery {
            scope: HistoryScope::Cwd,
            current_cwd: Some("/other".to_string()),
            ..context.clone()
        };
        assert!(!EntryMatcher::new(&elsewhere).matches(&entry, None));

        let other_session = HistoryQuery {
            scope: HistoryScope::Session,
            current_session_id: Some("session-b".to_string()),
            ..context.clone()
        };
        assert!(!EntryMatcher::new(&other_session).matches(&entry, None));

        let other_project = HistoryQuery {
            scope: HistoryScope::Project,
            current_project: Some("/elsewhere".to_string()),
            ..context
        };
        assert!(!EntryMatcher::new(&other_project).matches(&entry, None));
    }

    #[test]
    fn entry_matches_applies_status_and_duration() {
        let ok = sample_entry("ok", Some(0), Some(5000), None, None, None);
        let failed = sample_entry("bad", Some(2), Some(10), None, None, None);
        let unknown = sample_entry("legacy", None, None, None, None, None);

        let success = HistoryQuery {
            status: HistoryStatusFilter::Success,
            ..Default::default()
        };
        assert!(EntryMatcher::new(&success).matches(&ok, None));
        assert!(!EntryMatcher::new(&success).matches(&failed, None));
        assert!(!EntryMatcher::new(&success).matches(&unknown, None));

        let failure = HistoryQuery {
            status: HistoryStatusFilter::Failure,
            ..Default::default()
        };
        assert!(EntryMatcher::new(&failure).matches(&failed, None));
        assert!(!EntryMatcher::new(&failure).matches(&ok, None));
        // An entry with no recorded status is not a known failure.
        assert!(!EntryMatcher::new(&failure).matches(&unknown, None));

        let slow = HistoryQuery {
            min_duration_ms: Some(1000),
            ..Default::default()
        };
        assert!(EntryMatcher::new(&slow).matches(&ok, None));
        assert!(!EntryMatcher::new(&slow).matches(&failed, None));
        assert!(!EntryMatcher::new(&slow).matches(&unknown, None));
    }

    #[test]
    fn entry_matches_text_is_case_insensitive_with_and_without_cache() {
        let entry = sample_entry("Cargo Build", None, None, None, None, None);
        let query = HistoryQuery {
            text: Some("CARGO".to_string()),
            ..Default::default()
        };
        let matcher = EntryMatcher::new(&query);

        assert!(matcher.matches(&entry, None));
        // The cached path must agree with the on-the-fly one.
        assert!(matcher.matches(&entry, Some("cargo build")));
    }

    #[test]
    fn snapshot_entries_returns_newest_first_and_respects_the_cap() {
        let mut history = History::new();
        history
            .write_batch(vec![
                ("first".to_string(), 1),
                ("second".to_string(), 2),
                ("third".to_string(), 3),
            ])
            .unwrap();

        let snapshot = history.snapshot_entries(2);
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].entry, "third");
        assert_eq!(snapshot[1].entry, "second");
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

    #[test]
    fn history_navigation_filters_by_substring_and_restores_end() {
        let mut history = History::new();
        history
            .write_batch(vec![
                ("git status".to_string(), 1),
                ("cargo test".to_string(), 2),
                ("docker status".to_string(), 3),
            ])
            .unwrap();
        history.search_word = Some("status".to_string());

        assert_eq!(history.back().as_deref(), Some("docker status"));
        assert_eq!(history.back().as_deref(), Some("git status"));
        assert_eq!(history.back(), None);
        assert_eq!(history.forward().as_deref(), Some("docker status"));
        assert_eq!(history.forward(), None);
        assert!(history.at_end());
        assert_eq!(history.search_word.as_deref(), Some("status"));
    }

    #[test]
    fn history_navigation_uses_fish_smartcase_matching() {
        let mut history = History::new();
        history
            .write_batch(vec![
                ("Git Status".to_string(), 1),
                ("git status".to_string(), 2),
            ])
            .unwrap();

        history.search_word = Some("status".to_string());
        assert_eq!(history.back().as_deref(), Some("git status"));
        assert_eq!(history.back().as_deref(), Some("Git Status"));

        history.reset_index();
        history.search_word = Some("Status".to_string());
        assert_eq!(history.back().as_deref(), Some("Git Status"));
        assert_eq!(history.back(), None);
    }

    #[test]
    fn history_navigation_no_match_keeps_index_stable() {
        let mut history = History::new();
        history
            .write_batch(vec![
                ("git status".to_string(), 1),
                ("cargo test".to_string(), 2),
            ])
            .unwrap();
        history.search_word = Some("deploy".to_string());

        assert_eq!(history.back(), None);
        assert!(history.at_end());
        assert_eq!(history.forward(), None);
        assert!(history.at_end());
    }
}
