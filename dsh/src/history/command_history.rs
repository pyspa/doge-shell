//! Command history management.
//!
//! Provides the main command history storage with SQLite persistence,
//! background writing, and prefix-based search. Shared types and `History`'s
//! constructors live here; its methods are split by concern into
//! `command_history/{navigation,reload,persist,query}.rs`.

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

mod navigation;
mod persist;
mod query;
mod reload;

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
}

#[cfg(test)]
mod tests;
