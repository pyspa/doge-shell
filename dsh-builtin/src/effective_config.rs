//! Finite, read-only projection of current nonsecurity runtime preferences.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionMode {
    Off,
    Ghost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandLedgerMode {
    Off,
    Metadata,
    Output,
}

/// No arbitrary keys, user strings, or source-provenance claims.
/// Absence is represented by the capability's `None`, never guessed defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EffectiveConfig {
    pub suggestion_mode: SuggestionMode,
    pub transient_prompt: bool,
    pub auto_pair: bool,
    pub status_line: bool,
    pub failure_hint: bool,
    pub auto_notify_enabled: bool,
    /// Notification threshold in seconds.
    pub auto_notify_threshold: u64,
    pub command_ledger_mode: CommandLedgerMode,
    pub pipefail: bool,
}
