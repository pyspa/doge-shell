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

#[cfg(test)]
mod tests;

// Re-export main types for backward compatibility
pub use command_history::History;
pub use context::get_current_context;
pub use entry::Entry;
pub use frecency_history::FrecencyHistory;
