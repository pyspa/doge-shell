use crate::prompt::GitStatus;
use std::path::PathBuf;
// use parking_lot::RwLock;
// use std::sync::Arc; // We will need to re-export this or move it

#[derive(Debug, Clone)]
pub struct PromptContext {
    pub current_dir: PathBuf,
    pub git_root: Option<PathBuf>,
    pub git_status: Option<GitStatus>,
    pub rust_version: Option<String>,
    pub node_version: Option<String>,
    pub python_version: Option<String>,
    pub go_version: Option<String>,
}
