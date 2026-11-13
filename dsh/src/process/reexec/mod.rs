//! Versioned internal re-exec protocol: Rust code never runs after `fork`.
//!
//! Any child that must execute Rust (a background builtin today; isolated
//! subshell bodies in Phase 3) is spawned fresh instead of forked:
//!
//! ```text
//! parent shell
//!   → snapshot state (plain data, no live objects)
//!   → prefill and rewind an anonymous request file
//!   → reserve collision-free protocol descriptors from the kernel
//!   → posix_spawn(current_exe, --__dsh-internal-exec-fd=<N> [--__dsh-internal-status-fd=<M>])
//! child: fresh dogesh image
//!   → read request to EOF (size-capped)
//!   → fresh Tokio runtime, fresh Environment + Shell
//!   → run the already-materialized builtin argv, exit with its status
//! ```
//!
//! The request travels in an anonymous file duplicated to a dynamically
//! reserved descriptor (see [`fd_layout`]), never on the command line
//! (process-list and secret exposure) or in the environment (size limits,
//! inheritance). Internal protocol descriptors have no fixed numbers: their
//! targets are reserved atomically from the live process descriptor table
//! immediately before `posix_spawn` and remain reserved until spawn
//! completes. Internal protocol descriptors must never overwrite inherited
//! descriptors such as process-substitution `/dev/fd/N` handles.
//!
//! Delivery finishes before `posix_spawn`: a stopped helper cannot block the
//! parent on pipe capacity. No writer thread or post-spawn delivery wait exists.
//!
//! `config.lisp` is never re-run in the helper: the snapshot is
//! authoritative, so aliases and variables cannot drift or double-apply side
//! effects between parent and helper.

use crate::environment::child_snapshot::ChildShellSnapshot;
use crate::process::builtin::BuiltinProcess;
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use nix::sys::signal::{SigSet, Signal};
use nix::unistd::Pid;
use std::ffi::CString;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

mod fd_layout;
mod helper;
mod protocol;

pub use fd_layout::{INTERNAL_FD_MIN, InternalFdLayout, InternalHelperFds};
pub use helper::run_internal_helper;
pub use protocol::{
    BuiltinExecRequest, InternalExecKind, InternalExecRequest, MAX_INTERNAL_EXEC_REQUEST,
    NoCommandExecRequest, PROTOCOL_VERSION, PipelineSourceExecRequest, PlanExecMode,
    PlanExecRequest, PlanSignalPolicy, read_internal_request, validate_plan_signal_policy,
};

/// Spawn-time signal posture for one internal helper.
///
/// This is a spawn implementation detail, never serialized: the wire policy
/// is [`PlanSignalPolicy`]. `Normal` keeps the existing empty spawn mask;
/// `BlockInterruptAndQuitDuringInit` starts the child with SIGINT/SIGQUIT
/// blocked so the helper can install `SIG_IGN` before unblocking, closing
/// the spawn-to-`sigaction` race window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperSpawnSignalPolicy {
    Normal,
    BlockInterruptAndQuitDuringInit,
}

/// Whether this process runs under the async no-job-control signal
/// environment (SIGINT/SIGQUIT ignored from helper commit onward).
///
/// Set exactly once by an async-list helper when it commits
/// `PlanSignalPolicy::IgnoreInterruptAndQuit`, and inherited by any nested
/// helper that observes both dispositions already ignored, before any nested
/// spawn. Nested internal helpers consult it to preserve the ignored
/// dispositions instead of resetting them to default: POSIX requires the
/// commands in an async list to inherit the ignore, including pipeline
/// builtins, no-command stages, and substitutions that re-exec as nested
/// helpers. Top-level shells and `Normal` helpers never set it, so their
/// existing reset-to-default semantics are unchanged.
///
/// Test hygiene: never commit the `IgnoreInterruptAndQuit` path (or arrange
/// both-ignored dispositions) in-process — the cargo-test process shares
/// this flag and its own dispositions, so doing so would pollute every later
/// `spawn_sigdefault` expectation in the same process. The fail-closed
/// validation tests are in-process safe precisely because they return before
/// any signal commit.
static ASYNC_INT_QUIT_IGNORED: AtomicBool = AtomicBool::new(false);

/// Record that this process now runs under the async no-job-control signal
/// environment. Write-once: helpers execute a single request and exit.
fn mark_async_int_quit_ignored() {
    ASYNC_INT_QUIT_IGNORED.store(true, Ordering::SeqCst);
}

/// Whether nested helper spawns must preserve (rather than reset) the
/// inherited SIGINT/SIGQUIT dispositions.
fn async_int_quit_ignored() -> bool {
    ASYNC_INT_QUIT_IGNORED.load(Ordering::SeqCst)
}

/// The `posix_spawn` signal mask for one spawn policy. Pure: unit tests pin
/// the race protection without spawning processes.
fn spawn_sigmask(policy: HelperSpawnSignalPolicy) -> SigSet {
    let mut mask = SigSet::empty();
    if policy == HelperSpawnSignalPolicy::BlockInterruptAndQuitDuringInit {
        mask.add(Signal::SIGINT);
        mask.add(Signal::SIGQUIT);
    }
    mask
}

/// The `posix_spawn` default-reset set. SIGTSTP/SIGTTIN/SIGTTOU/SIGCHLD/
/// SIGPIPE are always reset so helpers never inherit the shell's ignored
/// dispositions. SIGINT/SIGQUIT are reset too — unless this process already
/// runs under the async no-job-control environment (`preserve_int_quit`),
/// in which case nested helpers inherit the required ignore instead of
/// reverting it to default. Pure: both arms are unit-tested.
fn spawn_sigdefault(preserve_int_quit: bool) -> SigSet {
    let mut sigdefault = SigSet::empty();
    if !preserve_int_quit {
        sigdefault.add(Signal::SIGINT);
        sigdefault.add(Signal::SIGQUIT);
    }
    for sig in [
        Signal::SIGTSTP,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
        Signal::SIGCHLD,
        Signal::SIGPIPE,
    ] {
        sigdefault.add(sig);
    }
    sigdefault
}

/// Resolve the binary a helper re-executes.
///
/// Production always re-executes `current_exe`. Under `cargo test` the
/// current image is a Cargo test harness, which cannot serve the
/// internal protocol — so tests locate the sibling `dogesh` binary instead
/// (built by a full `cargo test -p doge-shell`; `DOGESH_HELPER_BIN` overrides
/// everything for exotic layouts). A missing candidate is a clean `Err`,
/// never a silent fallback to re-parsing source.
fn helper_executable() -> Result<std::path::PathBuf> {
    if let Ok(path) = std::env::var("DOGESH_HELPER_BIN") {
        let path = std::path::PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("DOGESH_HELPER_BIN does not exist: {}", path.display());
    }
    let current = std::env::current_exe().context("cannot find this dogesh binary")?;
    if let Some(bin_dir) = cargo_test_bin_dir(&current) {
        let candidate = bin_dir.join("dogesh");
        if candidate.exists() {
            return Ok(candidate);
        }
        anyhow::bail!(
            "no dogesh helper binary next to the test harness; run a full `cargo test -p doge-shell` first"
        );
    }
    Ok(current)
}

/// Recognize both Cargo layouts without assuming the profile or target triple.
/// Separate build/target directories still require `DOGESH_HELPER_BIN`.
fn cargo_test_bin_dir(current: &std::path::Path) -> Option<&std::path::Path> {
    let dir = current.parent()?;
    if dir.file_name()? == "deps" {
        return dir.parent();
    }
    // New Cargo layout: <profile>/build/<package>/<hash>/out/<test>.
    if dir.file_name()? == "out" {
        let build = dir.parent()?.parent()?.parent()?;
        if build.file_name()? == "build" {
            return build.parent();
        }
    }
    None
}

/// Spawn a fresh `dogesh` helper carrying `request`, wired to the given
/// stdio, in process group `pgroup` (`Pid::from_raw(0)` = new group).
///
/// `request` must already be serialized and size-checked. `status_write`, when
/// given, is duplicated to an optional dynamically assigned status fd for
/// the helper's one-byte completion report. The protocol descriptor numbers
/// are reserved from the kernel immediately before spawn (see [`fd_layout`])
/// and passed through hidden argv; they never collide with inherited
/// descriptors such as process-substitution `/dev/fd/N` handles. Returns the
/// helper pid; the caller owns reaping it through the normal wait machinery,
/// which maps signal deaths to `128 + signal` without any duplicated logic.
///
/// The payload is written and rewound before spawn. Delivery errors therefore
/// leave no child to reap, and an externally stopped helper cannot block its
/// parent's launch on a full request pipe.
pub fn spawn_internal_helper(
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    request_bytes: &[u8],
    pgroup: Pid,
    status_write: Option<RawFd>,
    signal_policy: HelperSpawnSignalPolicy,
) -> Result<Pid> {
    spawn_internal_helper_admitted(
        ChildStdio {
            stdin,
            stdout,
            stderr,
        },
        request_bytes,
        pgroup,
        status_write,
        signal_policy,
        None,
    )
}

fn spawn_internal_helper_admitted(
    stdio: ChildStdio,
    request_bytes: &[u8],
    pgroup: Pid,
    status_write: Option<RawFd>,
    signal_policy: HelperSpawnSignalPolicy,
    admission: Option<&mut super::launch_admission::LaunchAdmission>,
) -> Result<Pid> {
    let ChildStdio {
        stdin,
        stdout,
        stderr,
    } = stdio;
    use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn};

    if request_bytes.len() > MAX_INTERNAL_EXEC_REQUEST {
        anyhow::bail!("internal exec request exceeds size limit");
    }
    let exe = helper_executable()?;
    let stage_admission = admission
        .as_ref()
        .map(|_| super::launch_admission::StageAdmission::for_helper())
        .transpose()?;
    let launch_targets = stage_admission
        .as_ref()
        .map(|stage| -> Result<_> {
            Ok((
                fd_layout::reserve_spawn_fd(stage.gate_read.as_fd(), INTERNAL_FD_MIN)?,
                fd_layout::reserve_spawn_fd(stage.ready_write.as_fd(), INTERNAL_FD_MIN)?,
            ))
        })
        .transpose()?;

    use std::io::{Seek as _, Write as _};
    let mut req_read = tempfile::tempfile().context("failed anonymous request file")?;
    req_read
        .write_all(request_bytes)
        .context("failed to write internal request")?;
    req_read
        .rewind()
        .context("failed to rewind internal request")?;
    // Reserve collision-free targets while every source and auxiliary
    // descriptor is still open. `layout` stays alive until `posix_spawn`
    // returns: dropping it earlier would recycle the numbers.
    // SAFETY: `status_write` is a borrowed caller-owned descriptor, only
    // probed by the reservation and referenced by the spawn actions.
    let status_source = status_write.map(|fd| unsafe { BorrowedFd::borrow_raw(fd) });
    let layout = InternalFdLayout::reserve(req_read.as_fd(), status_source)?;
    // Structural guarantee, spelled out: every descriptor the child may
    // already hold is open right now, so the kernel could not have picked
    // any of these numbers for the protocol targets.
    debug_assert!(![stdin, stdout, stderr].contains(&layout.request_fd()));
    if let Some(status_target) = layout.status_fd() {
        debug_assert!(![stdin, stdout, stderr].contains(&status_target));
    }

    // `NixPath` is implemented for `PathBuf`, not `CString`.
    let exe_cstr = CString::new(exe.as_os_str().as_encoded_bytes())
        .context("dogesh path is not a valid CString")?;
    // Only descriptor numbers cross on argv — never payload or secrets.
    let mut argv = vec![exe_cstr];
    argv.push(
        CString::new(format!("--__dsh-internal-exec-fd={}", layout.request_fd()))
            .expect("internal flag has no NUL"),
    );
    if let Some(status_target) = layout.status_fd() {
        argv.push(
            CString::new(format!("--__dsh-internal-status-fd={status_target}"))
                .expect("internal flag has no NUL"),
        );
    }

    if let Some((gate, ready)) = &launch_targets {
        argv.push(CString::new(format!(
            "--__dsh-internal-launch-gate-fd={}",
            gate.as_raw_fd()
        ))?);
        argv.push(CString::new(format!(
            "--__dsh-internal-launch-ready-fd={}",
            ready.as_raw_fd()
        ))?);
    }
    let background_snapshot = serde_json::from_slice::<InternalExecRequest>(request_bytes)
        .ok()
        .filter(|request| matches!(request.kind, InternalExecKind::ChatTask(_)))
        .map(|request| request.snapshot);
    if background_snapshot.is_some() {
        argv.push(CString::new("--__dsh-background-worker").expect("static flag"));
    }

    // Ordinary helpers inherit ambient state. Background chat workers receive
    // only their filtered snapshot, including provider-only credentials. Entries with interior NUL cannot cross `execve` and are
    // skipped rather than failing the spawn.
    let helper_environment: Vec<_> = match background_snapshot {
        Some(snapshot) => snapshot
            .variables
            .into_iter()
            .map(|(key, value)| {
                (
                    std::ffi::OsString::from(key),
                    std::ffi::OsString::from(value),
                )
            })
            .collect(),
        None => std::env::vars_os().collect(),
    };
    let envp: Vec<CString> = helper_environment
        .into_iter()
        .filter_map(|(key, value)| {
            let mut bytes = key.as_encoded_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_encoded_bytes());
            CString::new(bytes).ok()
        })
        .collect();

    let mut actions =
        PosixSpawnFileActions::init().context("failed to build spawn file actions")?;
    // Order: stdio first, then the internal request dup, then the optional
    // status dup, then the source-end closes (child-side only).
    for (from, to) in [
        (stdin, libc::STDIN_FILENO),
        (stdout, libc::STDOUT_FILENO),
        (stderr, libc::STDERR_FILENO),
    ] {
        actions
            .add_dup2(from, to)
            .context("failed to build spawn file actions")?;
    }
    actions
        .add_dup2(req_read.as_fd().as_raw_fd(), layout.request_fd())
        .context("failed to build spawn file actions")?;
    if let (Some(source), Some(target)) = (status_write, layout.status_fd()) {
        actions
            .add_dup2(source, target)
            .context("failed to build spawn file actions")?;
    }
    actions
        .add_close(req_read.as_fd().as_raw_fd())
        .context("failed to build spawn file actions")?;
    if let Some(source) = status_write {
        actions
            .add_close(source)
            .context("failed to build spawn file actions")?;
    }

    if let (Some(stage), Some((gate, ready))) = (&stage_admission, &launch_targets) {
        actions.add_dup2(stage.gate_read.as_raw_fd(), gate.as_raw_fd())?;
        actions.add_dup2(stage.ready_write.as_raw_fd(), ready.as_raw_fd())?;
    }
    let mut attr = PosixSpawnAttr::init().context("spawn attr")?;
    let mut spawn_flags = PosixSpawnFlags::empty();
    spawn_flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETPGROUP);
    spawn_flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF);
    spawn_flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK);
    attr.set_flags(spawn_flags).context("spawn flags")?;
    attr.set_pgroup(pgroup).context("spawn pgroup")?;
    // Normal helpers reset the job-control set to default so they never
    // inherit the shell's ignored dispositions. Async no-job-control plan
    // helpers additionally start with SIGINT/SIGQUIT blocked; the helper
    // converts them to `SIG_IGN` before unblocking and executing the plan,
    // so no `SIG_DFL` delivery window exists after spawn. Nested helpers
    // spawned inside that environment preserve the inherited ignore instead
    // of resetting it (see `spawn_sigdefault`).
    let sigdefault = spawn_sigdefault(async_int_quit_ignored());
    attr.set_sigdefault(&sigdefault)
        .context("spawn sigdefault")?;
    attr.set_sigmask(&spawn_sigmask(signal_policy))
        .context("spawn sigmask")?;

    let child = match posix_spawn(&exe, &actions, &attr, &argv, &envp) {
        Ok(child) => child,
        Err(err) => {
            // The request file and `layout` reservations own their fds and drop
            // here with no manual cleanup branch.
            return Err(anyhow::anyhow!(err).context("failed to posix_spawn helper"));
        }
    };
    // The reservation served its purpose once spawn returned.
    drop(layout);
    // The child owns its duplicate; the parent's copy is no longer needed.
    drop(req_read);
    if let (Some(admission), Some(stage)) = (admission, stage_admission) {
        admission.admit(child, stage);
    }
    Ok(child)
}

/// Shared re-exec spawn: snapshot plus an already-materialized builtin argv
/// into a fresh helper wired to the given stdio. No policy check here; the
/// callers refuse session-bound builtins before reaching this.
fn spawn_reexec_builtin_helper(
    ctx: &Context,
    process: &BuiltinProcess,
    shell: &Shell,
    admission: Option<&mut super::launch_admission::LaunchAdmission>,
) -> Result<Pid> {
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: process.stage_environment.snapshot(&shell.environment),
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: process.name.clone(),
            argv: process.argv.clone(),
            env_overrides: process.env_overrides.clone(),
        }),
    };
    let bytes = serde_json::to_vec(&request).context("encode internal request")?;
    // First pipeline stage starts a new group; later stages join the job's.
    let pgroup = ctx.pgid.unwrap_or(Pid::from_raw(0));
    spawn_internal_helper_admitted(
        ChildStdio {
            stdin: process.stdin,
            stdout: process.stdout,
            stderr: process.stderr,
        },
        &bytes,
        pgroup,
        None,
        HelperSpawnSignalPolicy::Normal,
        admission,
    )
}

/// Background entry point replacing `fork_builtin_process`.
///
/// The builtin's argv is already materialized by the caller; it crosses to
/// the helper verbatim. Builtins that need the live parent session are
/// refused with an explicit error — never an unsafe `fork()` fallback.
pub(crate) fn spawn_background_builtin(
    ctx: &mut Context,
    process: &mut BuiltinProcess,
    shell: &mut Shell,
) -> Result<Pid> {
    // Unknown builtins fail closed: without an explicit policy the helper
    // must never run them.
    let background_mode = dsh_builtin::background_builtin_mode(&process.name)
        .ok_or_else(|| anyhow::anyhow!("unknown builtin background policy: {}", process.name))?;
    if background_mode == dsh_builtin::BackgroundBuiltinMode::ParentSessionRequired {
        let message = format!(
            "dogesh: {}: cannot run in background (needs the live shell session)\r\n",
            process.name
        );
        super::fork::write_process_stderr(process.stderr, message.as_bytes());
        anyhow::bail!("{} cannot run in background", process.name);
    }

    let child = spawn_reexec_builtin_helper(ctx, process, shell, None)?;
    process.pid = Some(child);
    // Bookkeeping for the current launch scope.
    // `Job::launch` restores `ctx.process_count` to the caller's entry
    // value on return.
    ctx.process_count += 1;
    Ok(child)
}

/// Isolated builtin spawn for foreground pipeline stages.
///
/// Shares the background re-exec implementation (snapshot, fresh helper,
/// materialized argv). Session-bound or unknown builtins fail closed here
/// too — pipeline preflight rejects them before any spawn, so reaching this
/// is a defense-in-depth error, never a parent fallback.
pub(crate) fn spawn_isolated_builtin(
    ctx: &mut Context,
    process: &mut BuiltinProcess,
    shell: &mut Shell,
    admission: Option<&mut super::launch_admission::LaunchAdmission>,
) -> Result<Pid> {
    let background_mode = dsh_builtin::background_builtin_mode(&process.name)
        .ok_or_else(|| anyhow::anyhow!("unknown builtin isolation policy: {}", process.name))?;
    if background_mode == dsh_builtin::BackgroundBuiltinMode::ParentSessionRequired {
        // Same `\r\n` as the background refusal: both go through
        // `write_process_stderr` to a possibly-PTY stderr.
        let message = format!(
            "dogesh: {}: cannot run in a pipeline (needs the live shell session)\r\n",
            process.name
        );
        super::fork::write_process_stderr(process.stderr, message.as_bytes());
        anyhow::bail!("{} cannot run in a pipeline", process.name);
    }
    let child = spawn_reexec_builtin_helper(ctx, process, shell, admission)?;
    process.pid = Some(child);
    // Bookkeeping for the current launch scope.
    // `Job::launch` restores `ctx.process_count` to the caller's entry
    // value on return.
    ctx.process_count += 1;
    Ok(child)
}

/// Spawn a no-command pipeline stage helper carrying assignments plus the
/// last command-substitution status.
///
/// Redirections are deliberately not part of the request: the parent
/// pipeline launch path already applied them exactly once before spawning,
/// so the helper receives final stdio.
pub(crate) fn spawn_no_command(
    ctx: &mut Context,
    shell: &Shell,
    process: &super::NoCommandProcess,
    admission: Option<&mut super::launch_admission::LaunchAdmission>,
) -> Result<Pid> {
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: process.stage_environment.snapshot(&shell.environment),
        kind: InternalExecKind::NoCommand(NoCommandExecRequest {
            assignments: process.assignments.clone(),
            last_command_substitution_status: process.last_command_substitution_status,
        }),
    };
    let bytes = serde_json::to_vec(&request).context("encode internal request")?;
    let pgroup = ctx.pgid.unwrap_or(Pid::from_raw(0));
    let child = spawn_internal_helper_admitted(
        ChildStdio {
            stdin: process.stdin,
            stdout: process.stdout,
            stderr: process.stderr,
        },
        &bytes,
        pgroup,
        None,
        HelperSpawnSignalPolicy::Normal,
        admission,
    )?;
    // Bookkeeping for the current launch scope.
    // `Job::launch` restores `ctx.process_count` to the caller's entry
    // value on return.
    ctx.process_count += 1;
    Ok(child)
}

/// Spawn a synthetic pipeline source helper carrying owned bytes.
pub(crate) fn spawn_pipeline_source(
    ctx: &mut Context,
    shell: &Shell,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    data: &str,
    admission: Option<&mut super::launch_admission::LaunchAdmission>,
) -> Result<Pid> {
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&shell.environment.read()),
        kind: InternalExecKind::PipelineSource(PipelineSourceExecRequest {
            data: data.to_string(),
        }),
    };
    let bytes = serde_json::to_vec(&request).context("encode internal request")?;
    let pgroup = ctx.pgid.unwrap_or(Pid::from_raw(0));
    let child = spawn_internal_helper_admitted(
        ChildStdio {
            stdin,
            stdout,
            stderr,
        },
        &bytes,
        pgroup,
        None,
        HelperSpawnSignalPolicy::Normal,
        admission,
    )?;
    // Bookkeeping for the current launch scope.
    // `Job::launch` restores `ctx.process_count` to the caller's entry
    // value on return.
    ctx.process_count += 1;
    Ok(child)
}

/// Standard fds one helper is wired to.
#[derive(Debug, Clone, Copy)]
pub struct ChildStdio {
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

/// Spawn a helper executing an isolated plan body.
///
/// Grouping follows the mode: command-substitution helpers join `pgroup`
/// (normally the shell's own group, so terminal signals reach them together
/// with the parent while their output is captured); process-substitution
/// producers are spawned with a fresh group and lead it, so the reaper's
/// group-kill reaches grandchildren that outlive the consumer. State
/// isolation comes from the fresh process image in both cases, never from
/// signal isolation.
pub fn spawn_plan_helper(
    snapshot: &ChildShellSnapshot,
    plan: &crate::shell::plan::ExecutionPlan,
    mode: PlanExecMode,
    signal_policy: PlanSignalPolicy,
    stdio: ChildStdio,
    pgroup: Pid,
    status_write: Option<RawFd>,
) -> Result<Pid> {
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: snapshot.clone(),
        kind: InternalExecKind::Plan(PlanExecRequest {
            plan: plan.clone(),
            mode,
            signal_policy,
        }),
    };
    let bytes = serde_json::to_vec(&request).context("encode internal request")?;
    // The wire policy doubles as the spawn posture: only the async
    // no-job-control policy needs the blocked-during-init mask. Every other
    // mode spawns with the historical empty mask.
    let spawn_signal_policy = match signal_policy {
        PlanSignalPolicy::Normal => HelperSpawnSignalPolicy::Normal,
        PlanSignalPolicy::IgnoreInterruptAndQuit => {
            HelperSpawnSignalPolicy::BlockInterruptAndQuitDuringInit
        }
    };
    spawn_internal_helper(
        stdio.stdin,
        stdio.stdout,
        stdio.stderr,
        &bytes,
        pgroup,
        status_write,
        spawn_signal_policy,
    )
}

#[cfg(test)]
pub(crate) use helper::await_launch_admission;

#[cfg(test)]
mod tests;
