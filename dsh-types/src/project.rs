use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
    pub last_accessed: u64,
    pub created_at: u64,
    pub description: Option<String>,
}

impl Project {
    pub fn new(name: String, path: PathBuf) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            name,
            path,
            last_accessed: now,
            created_at: now,
            description: None,
        }
    }

    pub fn update_timestamp(&mut self) {
        self.last_accessed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }
}
