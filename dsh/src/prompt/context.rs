use crate::prompt::GitStatus;
use std::path::PathBuf;
// use parking_lot::RwLock;
// use std::sync::Arc; // We will need to re-export this or move it

#[derive(Debug, Clone)]
pub struct PromptContext {
    pub current_dir: PathBuf,
    pub git_root: Option<PathBuf>,
    pub git_status: Option<GitStatus>,
    // Add other global context like timing, status here if needed
}
