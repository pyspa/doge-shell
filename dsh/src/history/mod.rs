//! History module for command and directory history.
//!
//! This module provides:
//! - Command history with SQLite persistence
//! - Frecency-based directory history with context-aware boosting
//! - Background writing for non-blocking history updates
//!
//! # Module Structure
//!
//! - [`entry`] - History entry structure
//! - [`context`] - Context detection (git root, cwd)
//! - [`command_history`] - Command history (History struct)
//! - [`frecency_history`] - Frecency-based history (FrecencyHistory struct)

mod command_history;
mod context;
mod entry;
mod frecency_history;
pub mod picker;

#[cfg(test)]
mod tests;

// Re-export main types for backward compatibility
pub use command_history::History;
pub use command_history::{
    CommandEvent, CommandLedgerMode, EntryMatcher, HistoryMetadata, HistoryQuery, HistoryScope,
    HistoryStatusFilter,
};
pub(crate) use command_history::{CommandHistoryReloadSnapshot, HistoryReloadApply};
pub use context::get_current_context;
pub use entry::Entry;
pub use frecency_history::FrecencyHistory;
pub(crate) use frecency_history::{FrecencyReloadApply, FrecencyReloadSnapshot};

/// A [`HistoryQuery`] carrying only the caller's current scope context.
///
/// The `Cwd`/`Project`/`Session` filters compare an entry against these fields,
/// so every caller must populate them the same way — the `history` builtin and
/// the Ctrl-R picker share this to stay consistent.
pub fn query_context(session_id: String) -> HistoryQuery {
    HistoryQuery {
        current_cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        current_project: get_current_context(),
        current_session_id: Some(session_id),
        ..Default::default()
    }
}
