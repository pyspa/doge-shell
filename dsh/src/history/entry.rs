//! Entry structure for command history.
//!
//! Represents a single history entry with command text, timestamp, and execution count.

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
}
