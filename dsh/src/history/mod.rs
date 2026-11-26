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
pub(crate) use command_history::{
    COMMAND_HISTORY_NAME, CommandHistoryReloadSnapshot, HistoryReloadApply,
};
pub use command_history::{
    CommandEvent, CommandLedgerMode, EntryMatcher, HistoryMetadata, HistoryQuery, HistoryScope,
    HistoryStatusFilter,
};
pub use context::get_current_context;
pub use entry::Entry;
pub use frecency_history::FrecencyHistory;
pub(crate) use frecency_history::{FrecencyReloadApply, FrecencyReloadSnapshot};

/// Whether recalling this input in dogesh would repeat a shell command.
pub(crate) fn is_command_history_input(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() || input.starts_with('!') {
        return false;
    }
    if input.starts_with('#')
        && input.lines().all(|line| {
            let line = line.trim_start();
            line.is_empty() || line.starts_with('#')
        })
    {
        return false;
    }
    // Tab expands AI generation requests without submitting them. The same
    // input submitted with Enter can be an ordinary glob or pipeline.
    !(input.starts_with(['b', '#', '\'', '"', '\\']) && standalone_block_explanation(input))
}

fn standalone_block_explanation(input: &str) -> bool {
    use crate::parser::{Rule, ShellParser};
    use pest::Parser;

    let Ok(mut parsed) = ShellParser::parse(Rule::completion_scan, input) else {
        return false;
    };
    let mut words = Vec::with_capacity(3);
    let mut ended = false;
    for part in parsed.next().unwrap().into_inner() {
        match part.as_rule() {
            Rule::comment | Rule::EOI => {}
            Rule::completion_newline => ended |= !words.is_empty(),
            Rule::completion_control if part.as_str() == ";" => ended = true,
            Rule::completion_word if !ended && words.len() < 3 => words.push(part.as_str()),
            _ => return false,
        }
    }
    let [name, action, index] = words.as_slice() else {
        return false;
    };
    let is_word = |raw: &str, name: &str| {
        raw == name || shell_words::split(raw).is_ok_and(|words| words == [name])
    };
    is_word(name, "blocks")
        && is_word(action, "explain")
        && shell_words::split(index).is_ok_and(
            |words| matches!(words.as_slice(), [index] if index.parse::<usize>().is_ok()),
        )
}

/// Old databases did not distinguish an interactive AI pipe from an executed
/// shell command. Keep its question out of recall without changing the row.
pub(crate) fn is_legacy_command_history_input(input: &str) -> bool {
    is_command_history_input(input)
        && !(input.contains("|!") && crate::ai_task_input::output_pipe(input).is_some())
}

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
