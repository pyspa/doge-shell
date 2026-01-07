use serde::{Deserialize, Serialize};

/// Represents a command snippet
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[derive(Default)]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub tags: Option<String>,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub use_count: i64,
}

