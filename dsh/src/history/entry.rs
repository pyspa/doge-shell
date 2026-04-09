//! Entry structure for command history.
//!
//! Represents a single history entry with command text, timestamp, execution count,
//! and optional execution metadata.

use serde::{Deserialize, Serialize};

/// A single command history entry.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Entry {
    /// The command text.
    pub entry: String,
    /// Unix timestamp when the command was executed.
    pub when: i64,
    /// Number of times this command has been executed.
    pub count: i64,
    /// Context key for the command, usually git root or current working directory.
    #[serde(default)]
    pub context: Option<String>,
    /// Most recent exit code for this command.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Most recent execution duration in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Most recent current working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Session identifier for the latest execution.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Hostname for the latest execution.
    #[serde(default)]
    pub hostname: Option<String>,
}
