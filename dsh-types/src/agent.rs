//! Durable, provider-independent agent task state. Grants are host-owned.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    InputRequired,
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskGrant {
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    /// Exact commands, never a prefix. These do not bypass hard denials.
    pub commands: Vec<String>,
    /// Exact function/arguments approval entries, not server-wide permission.
    pub mcp_calls: Vec<String>,
    pub network_hosts: Vec<String>,
    pub environment: Vec<String>,
    pub sandbox: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub criterion: String,
    pub evidence_event: Option<u64>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: String,
    pub goal: String,
    pub root: PathBuf,
    pub status: TaskStatus,
    pub grant: TaskGrant,
    pub criteria: Vec<Verification>,
    pub plan: Vec<String>,
    pub progress: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub time_budget_ms: u64,
    pub elapsed_ms: u64,
    pub stop_reason: Option<String>,
    pub checkpoint: Option<Value>,
    /// A crash between intent and result must not replay this operation.
    pub pending_operation: Option<Value>,
    pub created_at: i64,
}

impl AgentTask {
    pub fn verified(&self) -> bool {
        !self.criteria.is_empty()
            && self
                .criteria
                .iter()
                .all(|c| c.passed && c.evidence_event.is_some())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub sequence: u64,
    pub kind: String,
    pub data: Value,
}
