//! Host-owned, in-memory background-task permissions and approval state.
//!
//! This module performs no I/O and grants no OS isolation. The supervisor must
//! resolve paths (including symlinks), classify protected files, validate tool
//! arguments and verify sandbox enforcement before constructing an operation.
//! Revalidate that resolution and preimage immediately before executing it.
//! Never construct an operation's targets or digests from model assertions.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    ReadOnly,
    WorkspaceEdit,
    SandboxedTest,
}

impl Profile {
    fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::WorkspaceEdit => 1,
            Self::SandboxedTest => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub time_ms: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsage {
    pub elapsed_ms: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub output_bytes: u64,
}

/// Plain scope metadata. Environment *values* never belong in this structure.
/// `sandbox_verified` is host evidence, not a user/model-selected switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskScope {
    pub profile: Profile,
    pub cwd: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub environment_names: Vec<String>,
    /// Host-normalized sandbox destinations; empty means no command network.
    /// Provider transport and MCP admission are separate host authorities.
    pub network_hosts: Vec<String>,
    pub sandbox_verified: bool,
    pub budget: BudgetLimits,
}

fn normalized_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

impl TaskScope {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !normalized_absolute(&self.cwd)
            || self.read_roots.is_empty()
            || self
                .read_roots
                .iter()
                .chain(&self.write_roots)
                .any(|path| !normalized_absolute(path))
        {
            return Err("scope requires normalized absolute host-resolved paths");
        }
        if !self.can_read(&self.cwd) {
            return Err("working directory is outside readable roots");
        }
        if self.environment_names.iter().any(|name| {
            name.is_empty()
                || !name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                })
        }) {
            return Err("invalid environment variable name");
        }
        if self.network_hosts.iter().any(|host| {
            host.is_empty()
                || host.bytes().any(|byte| {
                    !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'-' | b':')
                })
        }) {
            return Err("network destinations must be explicit host-normalized names");
        }
        let budget = self.budget;
        if [
            budget.time_ms,
            budget.tool_calls,
            budget.tokens,
            budget.output_bytes,
        ]
        .contains(&0)
        {
            return Err("task budgets must be positive");
        }
        Ok(())
    }

    fn can_read(&self, path: &Path) -> bool {
        within(path, &self.read_roots) || within(path, &self.write_roots)
    }

    fn is_restriction_of(&self, old: &Self) -> bool {
        self.cwd == old.cwd
            && self.profile.rank() <= old.profile.rank()
            && (!self.sandbox_verified || old.sandbox_verified)
            && self.read_roots.iter().all(|path| old.can_read(path))
            && self
                .write_roots
                .iter()
                .all(|path| within(path, &old.write_roots))
            && self
                .environment_names
                .iter()
                .all(|name| old.environment_names.contains(name))
            && self
                .network_hosts
                .iter()
                .all(|host| old.network_hosts.contains(host))
            && self.budget.time_ms <= old.budget.time_ms
            && self.budget.tool_calls <= old.budget.tool_calls
            && self.budget.tokens <= old.budget.tokens
            && self.budget.output_bytes <= old.budget.output_bytes
    }
}

/// Full operation identity, retained for exact equality rather than a weak hash.
/// Digests must be collision-resistant host-computed digests, or exact encodings.
/// A missing preimage denotes creation; the host must verify continued absence.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub tool: String,
    pub args: Value,
    pub canonical_targets: Vec<PathBuf>,
    pub preimage_digest: Option<String>,
    pub content_digest: Option<String>,
}

// Tool arguments, contents, command lines and digests can contain secrets.
impl fmt::Debug for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Operation")
            .field("target_count", &self.canonical_targets.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub task_id: String,
    pub request_id: u64,
    pub epoch: u64,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    AutoAllow,
    InputRequired,
    Denied(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    InputRequired,
    Completed,
    Cancelled,
    TimedOut,
    BudgetExhausted,
}

/// Supervisor-owned state. Do not deserialize worker input into this authority.
#[derive(Debug)]
pub struct TaskState {
    task_id: String,
    generation: u64,
    scope: TaskScope,
    usage: BudgetUsage,
    status: TaskStatus,
    next_request_id: u64,
    pending: Option<ApprovalRequest>,
}

impl TaskState {
    pub fn new(task_id: String, scope: TaskScope) -> Result<Self, &'static str> {
        scope.validate()?;
        if task_id.trim().is_empty() {
            return Err("task id must not be empty");
        }
        Ok(Self {
            task_id,
            generation: 0,
            scope,
            usage: BudgetUsage::default(),
            status: TaskStatus::Running,
            next_request_id: 0,
            pending: None,
        })
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn scope(&self) -> &TaskScope {
        &self.scope
    }
    pub fn usage(&self) -> BudgetUsage {
        self.usage
    }
    pub fn status(&self) -> TaskStatus {
        self.status
    }
    pub fn pending(&self) -> Option<&ApprovalRequest> {
        self.pending.as_ref()
    }

    fn active(&self) -> bool {
        matches!(self.status, TaskStatus::Running | TaskStatus::InputRequired)
    }

    fn observe_time(&mut self, elapsed_ms: u64) {
        self.usage.elapsed_ms = self.usage.elapsed_ms.max(elapsed_ms);
        if self.active() && self.usage.elapsed_ms >= self.scope.budget.time_ms {
            self.stop(TaskStatus::TimedOut);
        }
    }

    fn stop(&mut self, status: TaskStatus) {
        self.pending = None;
        self.status = status;
    }

    /// `elapsed_ms` is a host monotonic duration since task admission.
    pub fn evaluate(&mut self, operation: &Operation, elapsed_ms: u64) -> PermissionDecision {
        self.observe_time(elapsed_ms);
        if !self.active() {
            return PermissionDecision::Denied("task is not active");
        }
        if self.pending.is_some() {
            return PermissionDecision::Denied("an approval is already pending");
        }
        self.classify(operation)
    }

    fn classify(&self, operation: &Operation) -> PermissionDecision {
        if !operation.args.is_object()
            || operation.canonical_targets.is_empty()
            || operation
                .canonical_targets
                .iter()
                .any(|path| !normalized_absolute(path))
        {
            return PermissionDecision::Denied(
                "operation requires validated arguments and targets",
            );
        }
        let readable = operation
            .canonical_targets
            .iter()
            .all(|path| self.scope.can_read(path));
        let writable = operation
            .canonical_targets
            .iter()
            .all(|path| within(path, &self.scope.write_roots));
        let auto = match operation.tool.as_str() {
            "read_file" | "ls" => readable,
            "edit" | "str_replace" => {
                if operation
                    .content_digest
                    .as_ref()
                    .is_none_or(|value| value.is_empty())
                {
                    return PermissionDecision::Denied(
                        "write operation requires a content identity",
                    );
                }
                self.scope.profile != Profile::ReadOnly && writable
            }
            "execute" => {
                self.scope.profile == Profile::SandboxedTest
                    && self.scope.sandbox_verified
                    && readable
            }
            _ => return PermissionDecision::Denied("unsupported background tool"),
        };
        if auto {
            PermissionDecision::AutoAllow
        } else {
            PermissionDecision::InputRequired
        }
    }

    pub fn request_approval(
        &mut self,
        operation: Operation,
        elapsed_ms: u64,
    ) -> Result<ApprovalRequest, &'static str> {
        match self.evaluate(&operation, elapsed_ms) {
            PermissionDecision::InputRequired => {}
            PermissionDecision::AutoAllow => return Err("operation does not need approval"),
            PermissionDecision::Denied(reason) => return Err(reason),
        }
        self.create_approval(operation)
    }

    /// Let the host require a one-time confirmation even inside the automatic
    /// profile (for example, after detecting sensitive content). Hard denials
    /// such as unknown tools and invalid targets remain non-overridable.
    pub fn request_manual_approval(
        &mut self,
        operation: Operation,
        elapsed_ms: u64,
    ) -> Result<ApprovalRequest, &'static str> {
        if let PermissionDecision::Denied(reason) = self.evaluate(&operation, elapsed_ms) {
            return Err(reason);
        }
        self.create_approval(operation)
    }

    fn create_approval(&mut self, operation: Operation) -> Result<ApprovalRequest, &'static str> {
        let request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or("approval id exhausted")?;
        self.next_request_id = request_id;
        let request = ApprovalRequest {
            task_id: self.task_id.clone(),
            request_id,
            epoch: self.generation,
            operation,
        };
        self.pending = Some(request.clone());
        self.status = TaskStatus::InputRequired;
        Ok(request)
    }

    /// Consume exactly once, immediately before the host executes `operation`.
    /// Even a mismatched response invalidates the pending request.
    pub fn consume_approval(
        &mut self,
        request: &ApprovalRequest,
        operation: &Operation,
        elapsed_ms: u64,
    ) -> Result<(), &'static str> {
        self.observe_time(elapsed_ms);
        let pending = self.pending.take();
        if !self.active() {
            return Err("task is not active");
        }
        self.status = TaskStatus::Running;
        if pending.as_ref() != Some(request)
            || request.task_id != self.task_id
            || request.epoch != self.generation
            || &request.operation != operation
        {
            return Err("approval is stale or operation changed");
        }
        Ok(())
    }

    pub fn deny_approval(&mut self) {
        self.pending = None;
        if self.active() {
            self.status = TaskStatus::Running;
        }
    }

    /// Usage values are increments except for the absolute monotonic elapsed time.
    /// Charge every attempted tool call, including denied/failed calls.
    pub fn record_usage(
        &mut self,
        tool_calls: u64,
        tokens: u64,
        output_bytes: u64,
        elapsed_ms: u64,
    ) {
        self.usage.tool_calls = self.usage.tool_calls.saturating_add(tool_calls);
        self.usage.tokens = self.usage.tokens.saturating_add(tokens);
        self.usage.output_bytes = self.usage.output_bytes.saturating_add(output_bytes);
        self.observe_time(elapsed_ms);
        if self.active()
            && (self.usage.tool_calls >= self.scope.budget.tool_calls
                || self.usage.tokens >= self.scope.budget.tokens
                || self.usage.output_bytes >= self.scope.budget.output_bytes)
        {
            self.stop(TaskStatus::BudgetExhausted);
        }
    }

    /// No authority expansion is allowed. Caller must stop already-running
    /// commands whose previous sandbox/grants no longer fit the new scope.
    pub fn restrict_scope(
        &mut self,
        scope: TaskScope,
        elapsed_ms: u64,
    ) -> Result<(), &'static str> {
        self.observe_time(elapsed_ms);
        if !self.active() {
            return Err("task is not active");
        }
        scope.validate()?;
        if !scope.is_restriction_of(&self.scope) {
            return Err("scope update would expand authority");
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("scope generation exhausted")?;
        self.scope = scope;
        self.pending = None;
        self.status = TaskStatus::Running;
        self.record_usage(0, 0, 0, elapsed_ms);
        Ok(())
    }

    pub fn cancel(&mut self) {
        if self.active() {
            self.stop(TaskStatus::Cancelled);
        }
    }
    pub fn complete(&mut self, elapsed_ms: u64) {
        self.observe_time(elapsed_ms);
        if self.active() {
            self.stop(TaskStatus::Completed);
        }
    }
    pub fn check_deadline(&mut self, elapsed_ms: u64) {
        self.observe_time(elapsed_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope(profile: Profile) -> TaskScope {
        TaskScope {
            profile,
            cwd: "/workspace".into(),
            read_roots: vec!["/workspace".into()],
            write_roots: vec!["/workspace".into()],
            environment_names: vec!["LANG".into()],
            network_hosts: Vec::new(),
            sandbox_verified: false,
            budget: BudgetLimits {
                time_ms: 100,
                tool_calls: 10,
                tokens: 1000,
                output_bytes: 1024,
            },
        }
    }
    fn operation(tool: &str, path: &str) -> Operation {
        Operation {
            tool: tool.into(),
            args: json!({"secret": "credential-value"}),
            canonical_targets: vec![path.into()],
            preimage_digest: Some("before".into()),
            content_digest: Some("after".into()),
        }
    }
    fn task(profile: Profile) -> TaskState {
        TaskState::new("task-a".into(), scope(profile)).unwrap()
    }

    #[test]
    fn profile_and_target_matrix_fails_closed() {
        for (profile, tool, path, verified, expected) in [
            (
                Profile::ReadOnly,
                "read_file",
                "/workspace/file",
                false,
                PermissionDecision::AutoAllow,
            ),
            (
                Profile::ReadOnly,
                "ls",
                "/workspace",
                false,
                PermissionDecision::AutoAllow,
            ),
            (
                Profile::ReadOnly,
                "edit",
                "/workspace/file",
                false,
                PermissionDecision::InputRequired,
            ),
            (
                Profile::WorkspaceEdit,
                "str_replace",
                "/workspace/file",
                false,
                PermissionDecision::AutoAllow,
            ),
            (
                Profile::WorkspaceEdit,
                "edit",
                "/workspace-other/file",
                false,
                PermissionDecision::InputRequired,
            ),
            (
                Profile::SandboxedTest,
                "execute",
                "/workspace",
                false,
                PermissionDecision::InputRequired,
            ),
            (
                Profile::SandboxedTest,
                "execute",
                "/workspace",
                true,
                PermissionDecision::AutoAllow,
            ),
            (
                Profile::WorkspaceEdit,
                "execute",
                "/workspace",
                true,
                PermissionDecision::InputRequired,
            ),
        ] {
            let mut scope = scope(profile);
            scope.sandbox_verified = verified;
            let mut state = TaskState::new("task".into(), scope).unwrap();
            assert_eq!(
                state.evaluate(&operation(tool, path), 0),
                expected,
                "{profile:?}/{tool}/{verified}"
            );
        }
        for op in [
            operation("unknown", "/workspace"),
            operation("read_file", "relative"),
            operation("edit", "/workspace/../outside"),
        ] {
            let mut state = task(Profile::SandboxedTest);
            assert!(matches!(
                state.evaluate(&op, 0),
                PermissionDecision::Denied(_)
            ));
            assert!(state.request_approval(op, 0).is_err());
        }
    }

    #[test]
    fn approval_binds_every_operation_field_and_is_single_use() {
        let original = operation("edit", "/workspace/file");
        for field in 0..7 {
            let mut state = task(Profile::ReadOnly);
            let mut request = state.request_approval(original.clone(), 0).unwrap();
            let mut changed = original.clone();
            match field {
                0 => changed.tool = "str_replace".into(),
                1 => changed.args = json!({"secret": "changed"}),
                2 => changed.canonical_targets = vec!["/workspace/other".into()],
                3 => changed.preimage_digest = Some("other".into()),
                4 => changed.content_digest = Some("other".into()),
                5 => request.task_id = "task-b".into(),
                _ => request.request_id += 1,
            }
            assert!(state.consume_approval(&request, &changed, 1).is_err());
            assert!(state.pending().is_none());
        }
        let mut state = task(Profile::ReadOnly);
        let request = state.request_approval(original.clone(), 0).unwrap();
        assert!(state.consume_approval(&request, &original, 1).is_ok());
        assert!(state.consume_approval(&request, &original, 1).is_err());
    }

    #[test]
    fn cancellation_deadline_and_scope_changes_invalidate_approvals() {
        let op = operation("execute", "/workspace");
        for transition in 0..4 {
            let mut state = task(Profile::WorkspaceEdit);
            let request = state.request_approval(op.clone(), 0).unwrap();
            match transition {
                0 => state.cancel(),
                1 => state.check_deadline(100),
                2 => {
                    let mut smaller = scope(Profile::ReadOnly);
                    smaller.write_roots.clear();
                    state.restrict_scope(smaller, 1).unwrap();
                }
                _ => state.record_usage(10, 0, 0, 1),
            }
            assert!(state.pending().is_none());
            assert!(state.consume_approval(&request, &op, 1).is_err());
        }
        let mut state = task(Profile::ReadOnly);
        assert!(
            state
                .restrict_scope(scope(Profile::WorkspaceEdit), 0)
                .is_err()
        );
        assert_eq!(state.generation(), 0);
    }

    #[test]
    fn all_budgets_are_bounded_and_elapsed_time_never_rewinds() {
        for usage in [
            (10, 0, 0, 0),
            (0, 1000, 0, 0),
            (0, 0, 1024, 0),
            (0, 0, 0, 100),
        ] {
            let mut state = task(Profile::ReadOnly);
            state.record_usage(usage.0, usage.1, usage.2, usage.3);
            assert!(!state.active());
            assert!(matches!(
                state.evaluate(&operation("read_file", "/workspace/file"), 0),
                PermissionDecision::Denied(_)
            ));
        }
        let mut state = task(Profile::ReadOnly);
        state.check_deadline(99);
        state.check_deadline(0);
        assert_eq!(state.usage().elapsed_ms, 99);
    }

    #[test]
    fn scope_restriction_and_validation_cover_each_authority_dimension() {
        for dimension in 0..9 {
            let mut state = task(Profile::WorkspaceEdit);
            let mut invalid = state.scope().clone();
            match dimension {
                0 => invalid.cwd = "/workspace/sub".into(),
                1 => invalid.read_roots.push("/elsewhere".into()),
                2 => invalid.write_roots.push("/elsewhere".into()),
                3 => invalid.environment_names.push("TOKEN".into()),
                4 => invalid.sandbox_verified = true,
                5 => invalid.budget.time_ms += 1,
                6 => invalid.budget.tokens += 1,
                7 => invalid.profile = Profile::SandboxedTest,
                _ => invalid.network_hosts.push("example.com".into()),
            }
            assert!(state.restrict_scope(invalid, 0).is_err());
        }
        for path in ["relative", "/workspace/../outside"] {
            let mut invalid = scope(Profile::ReadOnly);
            invalid.read_roots = vec![path.into()];
            assert!(TaskState::new("task".into(), invalid).is_err());
        }
        let mut invalid = scope(Profile::ReadOnly);
        invalid.environment_names = vec!["TOKEN=value".into()];
        assert!(TaskState::new("task".into(), invalid).is_err());
    }

    #[test]
    fn debug_never_renders_operation_payload() {
        let mut state = task(Profile::ReadOnly);
        state
            .request_approval(operation("execute", "/workspace"), 0)
            .unwrap();
        let rendered = format!("{state:?}");
        for secret in ["credential-value", "before", "after", "secret"] {
            assert!(!rendered.contains(secret));
        }
    }

    #[test]
    fn host_manual_approval_can_strengthen_but_never_bypass_the_profile() {
        let mut state = task(Profile::WorkspaceEdit);
        let op = operation("edit", "/workspace/file");
        assert!(state.request_approval(op.clone(), 0).is_err());
        let request = state.request_manual_approval(op.clone(), 0).unwrap();
        assert_eq!(state.status(), TaskStatus::InputRequired);
        assert!(state.consume_approval(&request, &op, 1).is_ok());
        assert!(
            state
                .request_manual_approval(operation("unknown", "/workspace"), 1)
                .is_err()
        );
        state.cancel();
        assert!(state.request_manual_approval(op, 1).is_err());
    }
}
