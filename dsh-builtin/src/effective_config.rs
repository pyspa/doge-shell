//! Finite, read-only projection of current nonsecurity runtime preferences.
use serde::Serialize;
use std::path::PathBuf;

/// Outcome of the most recent attempt to load config.lisp in this shell.
/// Diagnostics do not retain raw errors, which can contain config values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLoadStatus {
    Loaded,
    Missing,
    ReadFailed,
    EvaluationFailed,
}

#[derive(Debug, Clone)]
pub struct ConfigLoadReport {
    pub path: PathBuf,
    pub status: ConfigLoadStatus,
}

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
