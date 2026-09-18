//! Side-effect-free execution plan: what parsing produces before anything runs.
//!
//! `ExecutionPlan` is pure data (no pids, fds, processes). The evaluator gates
//! on `&&`/`||` first, then materializes only the selected jobs, authorizing
//! each substitution body and each final argv through `SafetyGuard`.

use crate::process::{ListOp, Redirect, SubshellType};

/// A whole input line, parsed without running anything.
#[derive(Debug, Clone, Default)]
pub struct ExecutionPlan {
    pub jobs: Vec<PlannedJob>,
}

impl ExecutionPlan {
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

/// One `;`/`&&`/`||`-separated job: a pipeline plus its gating and flags.
#[derive(Debug, Clone)]
pub struct PlannedJob {
    /// User-facing source text (for safety messages and `Job.cmd`).
    pub source: String,
    /// Pipeline stages in execution order.
    pub stages: Vec<PlannedCommand>,
    /// Separator after this job: None / && / ||.
    pub list_op: ListOp,
    pub foreground: bool,
    pub capture_output: bool,
    pub struct_pipe_exprs: Vec<String>,
    pub subshell: SubshellType,
}

impl PlannedJob {
    pub fn contains_deferred_evaluation(&self) -> bool {
        self.stages.iter().any(|stage| {
            stage
                .argv
                .iter()
                .any(|arg| matches!(arg, PlannedArg::Substitution(_)))
        })
    }

    pub fn is_assignment_only(&self) -> bool {
        !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|stage| stage.argv.is_empty() && !stage.env_overrides.is_empty())
    }
}

/// One pipeline stage: argv template plus per-process redirections and env.
#[derive(Debug, Clone)]
pub struct PlannedCommand {
    pub argv: Vec<PlannedArg>,
    pub redirects: Vec<Redirect>,
    pub env_overrides: Vec<(String, String)>,
}

impl PlannedCommand {
    pub fn is_empty(&self) -> bool {
        self.argv.is_empty() && self.redirects.is_empty() && self.env_overrides.is_empty()
    }
}

/// A literal word, or a deferred `$(...)` / `<(...)` / `(...)` body.
#[derive(Debug, Clone)]
pub enum PlannedArg {
    Literal(String),
    Substitution(PlannedSubstitution),
}

/// A deferred substitution body: its own plan, evaluated only after gating
/// and authorization.
#[derive(Debug, Clone)]
pub struct PlannedSubstitution {
    pub source: String,
    pub kind: SubshellType,
    pub plan: Box<ExecutionPlan>,
}
