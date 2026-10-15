//! Host-owned RPC and operation admission for an isolated chat task.
use super::ApprovalDecision;
use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Process-local background task authority shared by worker and supervisor.
/// RPC forwarding and exact-operation admission must precede local dispatch;
/// this context never broadens the shell's existing safety policy.
pub trait BackgroundTaskContext: Send + Sync {
    /// Worker: forward through the parent. Parent host: return None.
    fn remote_tool(&self, name: &str, args: &str) -> Option<Result<String, String>>;
    /// Whether the supervisor admitted this exact local operation.
    fn permits_tool(&self, name: &str, args: &str) -> bool;
    fn confirm(&self, message: &str) -> Result<ApprovalDecision>;
    fn is_canceled(&self) -> bool;
    fn cwd(&self) -> PathBuf;
    fn jobs(&self) -> Option<Arc<parking_lot::Mutex<crate::agent::jobs::AgentJobs>>>;
    /// Exact host-admitted target, checked again after tool path resolution.
    fn read_authorized(&self, path: &Path) -> bool;
    /// Verify bytes actually read, before previewing or exposing them.
    fn read_result_authorized(&self, path: &Path, contents: &str) -> bool;
    fn write_authorized(&self, path: &Path) -> bool;
    /// Verify the exact bytes about to be written as well as the preimage.
    fn write_contents_authorized(&self, path: &Path, contents: &str) -> bool;
}
