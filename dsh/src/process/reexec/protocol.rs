//! Request/response shapes for the internal re-exec protocol.
//!
//! The request is plain data (snapshot + already-materialized work item)
//! serialized to JSON and delivered on a dynamically reserved pipe; the
//! descriptor numbers themselves are transport metadata owned by
//! [`super::fd_layout`], never part of this payload.

use crate::environment::child_snapshot::ChildShellSnapshot;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::io::RawFd;

/// Protocol version. Unknown versions fail closed.
///
/// Bumped to 2 when `ExecutionPlan` moved from a flat `jobs` list to
/// `lists` of AND-OR lists and `PlanExecMode` gained `AsyncAndOrList`.
/// Bumped to 3 for materialized no-command pipeline stage helper support
/// (`InternalExecKind::NoCommand`).
/// Bumped to 4: `PlanExecRequest` carries an explicit `PlanSignalPolicy`
/// because async AND-OR signal semantics depend on the caller's job-control
/// state, which a non-interactive helper cannot reconstruct.
/// Bumped to 5: `ChildShellSnapshot` carries `ShellOptions` / pipefail
/// state, so re-exec helpers inherit `set -o pipefail`.
/// Bumped to 6: unified shell variable/export namespace; `ChildShellSnapshot`
/// no longer carries `system_env_vars`.
/// Bumped to 7: `PlannedSubstitution` carries explicit input/output
/// process-substitution direction (`PlannedSubstitutionKind::Process(Read)`
/// vs `Process(Write)`).
/// Bumped to 8: `ExecutionPlan` / `WordPart` carries structured modified
/// parameter expansion (`WordPart::ParameterExpansion` with
/// `PlannedParameterExpansion`).
/// Bumped to 9: `ExecutionPlan` / `WordPart` carries structured arithmetic
/// expansion (`WordPart::ArithmeticExpansion` with
/// `PlannedArithmeticExpansion`).
/// Bumped to 10: ExecutionPlan/WordPart and ChildShellSnapshot carry invocation parameters.
/// Bumped to 11 for the private background-chat worker request.
/// Bumped to 12 for paired launch readiness/consumed-token acknowledgement.
pub const PROTOCOL_VERSION: u32 = 12;
/// Upper bound for one request; the child never does an unbounded
/// `read_to_end`. Oversized input is rejected with a non-zero exit.
pub const MAX_INTERNAL_EXEC_REQUEST: usize = 8 * 1024 * 1024;

/// What a helper process should execute. Extended with a plan variant in
/// Phase 3; keep every variant structured and already-materialized — never a
/// source string for `dogesh -c` re-parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalExecRequest {
    pub version: u32,
    pub snapshot: ChildShellSnapshot,
    pub kind: InternalExecKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InternalExecKind {
    ChatTask(Box<crate::background_task::BackgroundWorkerRequest>),
    Builtin(BuiltinExecRequest),
    Plan(PlanExecRequest),
    PipelineSource(PipelineSourceExecRequest),
    NoCommand(NoCommandExecRequest),
}

/// An isolated shell-execution body: `$(...)`, `<(...)`, or `( ... )`.
/// Structured and side-effect-free — the helper runs it through the same
/// gate → materialize → authorize → launch evaluator as the top level, never
/// by re-parsing source with `dogesh -c`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecRequest {
    pub plan: crate::shell::plan::ExecutionPlan,
    pub mode: PlanExecMode,
    pub signal_policy: PlanSignalPolicy,
}

/// The stdio/capture contract for one plan execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanExecMode {
    /// `( ... )` statement group: stdio inherited, status becomes the
    /// helper's exit code.
    Subshell,
    /// `$(...)`: stdout is captured data; the helper's exit code is
    /// informational only (matches the historical in-process behavior where
    /// the inner status only steered inner gating).
    CommandSubstitution,
    /// `<(...)` / `>(...)` process-substitution helper: stdout is the
    /// producer stream for `Read`, stdin is the consumer stream for `Write`.
    /// The parent wires `ChildStdio` for the direction; the helper's
    /// execution environment is identical either way.
    ProcessSubstitution,
    /// `list &`: one AND-OR list runs as a managed background job. The
    /// request carries the list normalized to foreground (see
    /// `PlannedAndOrList::isolated_body_plan`); the helper evaluates it as
    /// an ordinary chain, so it never re-spawns itself asynchronously.
    AsyncAndOrList,
}

/// An already-materialized builtin invocation: no re-parse, no alias pass,
/// no runtime expansion, no second `SafetyGuard` round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinExecRequest {
    pub name: String,
    pub argv: Vec<String>,
    pub env_overrides: Vec<(String, String)>,
}

/// The initial signal disposition contract for one plan execution.
///
/// A plan request includes an execution signal policy because async AND-OR
/// signal semantics depend on the caller's job-control state, which cannot
/// be reconstructed by the non-interactive helper: `supports_job_control()`
/// is false inside every helper, so the parent must capture its own
/// decision at the spawn boundary and carry it here explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanSignalPolicy {
    /// Existing semantics: no helper-side signal change. Used by every
    /// substitution/subshell helper and by job-control-enabled async lists.
    Normal,
    /// Async AND-OR list without job control: the helper starts with
    /// SIGINT/SIGQUIT blocked at spawn, installs `SIG_IGN` for both, then
    /// unblocks before evaluating the plan (POSIX.1-2024 async-list
    /// semantics). Only valid with `PlanExecMode::AsyncAndOrList`.
    IgnoreInterruptAndQuit,
}

/// Validate a mode/policy combination. `IgnoreInterruptAndQuit` is an
/// async-list-only policy: any other mode carrying it is a malformed
/// internal request and must fail closed, never execute.
pub fn validate_plan_signal_policy(mode: PlanExecMode, policy: PlanSignalPolicy) -> Result<()> {
    if policy == PlanSignalPolicy::IgnoreInterruptAndQuit && mode != PlanExecMode::AsyncAndOrList {
        anyhow::bail!("invalid signal policy {policy:?} for plan mode {mode:?}");
    }
    Ok(())
}

/// A synthetic pipeline head: finite bytes the helper writes to fd 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSourceExecRequest {
    pub data: String,
}

/// An already-materialized no-command pipeline stage: assignments plus the
/// last command-substitution status. Redirections are deliberately absent:
/// redirections are already applied exactly once by the parent pipeline
/// launch path before spawning the helper; the helper receives final stdio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoCommandExecRequest {
    pub assignments: Vec<(String, String)>,
    pub last_command_substitution_status: Option<i32>,
}

/// Read one request from `fd` to EOF, enforcing the size cap and version.
///
/// Takes ownership of `fd` and closes it on return, success or failure.
/// Malformed input (bad fd, oversized payload, invalid JSON, unknown
/// version) is a clean `Err`, never a panic — the helper request is a trust
/// boundary, and a user hand-invoking the hidden flag just gets a non-zero
/// exit, never privilege escalation.
pub fn read_internal_request(fd: RawFd) -> Result<InternalExecRequest> {
    if !(3..).contains(&fd) {
        anyhow::bail!("invalid internal exec fd");
    }
    // Raw `read(2)` on a borrowed fd: `File::from_raw_fd` would claim
    // ownership and abort on `close` for a bogus fd, but a user hand-invoking
    // the hidden flag deserves a clean error and a non-zero exit instead.
    // Every early `bail!` below closes `fd` first: the caller hands over
    // ownership on entry, and the helper exits right after either way.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
        if n < 0 {
            let errno = std::io::Error::last_os_error();
            if errno.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            unsafe { libc::close(fd) };
            anyhow::bail!("failed to read internal exec request: {errno}");
        }
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if buf.len() > MAX_INTERNAL_EXEC_REQUEST {
            unsafe { libc::close(fd) };
            anyhow::bail!("internal exec request exceeds size limit");
        }
    }
    unsafe { libc::close(fd) };
    let request: InternalExecRequest =
        serde_json::from_slice(&buf).context("invalid internal exec request")?;
    if request.version != PROTOCOL_VERSION {
        anyhow::bail!("unsupported internal exec version: {}", request.version);
    }
    Ok(request)
}

/// Test-only builders shared with the helper tests.
#[cfg(test)]
pub(crate) fn pipe_with_request(request: &InternalExecRequest) -> RawFd {
    use std::io::Write as _;
    use std::os::fd::FromRawFd as _;
    use std::os::fd::IntoRawFd as _;
    let bytes = serde_json::to_vec(request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    read.into_raw_fd()
}

#[cfg(test)]
pub(crate) fn builtin_request(name: &str, argv: Vec<String>) -> InternalExecRequest {
    let env_arc = crate::environment::Environment::new();
    InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&env_arc.read()),
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: name.to_string(),
            argv,
            env_overrides: vec![],
        }),
    }
}

/// Test-only plan request with an explicit mode/policy pair, for
/// fail-closed validation tests (including malformed combinations no
/// production call site ever builds).
#[cfg(test)]
pub(crate) fn plan_request_for_test(
    mode: PlanExecMode,
    signal_policy: PlanSignalPolicy,
) -> InternalExecRequest {
    let env_arc = crate::environment::Environment::new();
    InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&env_arc.read()),
        kind: InternalExecKind::Plan(PlanExecRequest {
            plan: crate::shell::plan::ExecutionPlan { lists: Vec::new() },
            mode,
            signal_policy,
        }),
    }
}

#[cfg(test)]
mod tests;
