use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct FileContextCache {
    pub path: std::path::PathBuf,
    pub files: Arc<Vec<String>>,
    pub updated_at: Option<Instant>,
}

impl FileContextCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_valid(&self, cwd: &std::path::Path) -> bool {
        if self.path != cwd {
            return false;
        }
        if let Some(t) = self.updated_at {
            // 2 seconds validity
            if t.elapsed() < Duration::from_secs(2) {
                return true;
            }
        }
        false
    }
}
