//! Versioned internal re-exec protocol: Rust code never runs after `fork`.
//!
//! Any child that must execute Rust (a background builtin today; isolated
//! subshell bodies in Phase 3) is spawned fresh instead of forked:
//!
//! ```text
//! parent shell
//!   → snapshot state (plain data, no live objects)
//!   → reserve collision-free protocol descriptors from the kernel
//!   → posix_spawn(current_exe, --__dsh-internal-exec-fd=<N> [--__dsh-internal-status-fd=<M>])
//!   → write JSON request to the pipe, close
//! child: fresh dogesh image
//!   → read request to EOF (size-capped)
//!   → fresh Tokio runtime, fresh Environment + Shell
//!   → run the already-materialized builtin argv, exit with its status
//! ```
//!
//! The request travels on a dedicated pipe duplicated to a dynamically
//! reserved descriptor (see [`fd_layout`]), never on the command line
//! (process-list and secret exposure) or in the environment (size limits,
//! inheritance). Internal protocol descriptors have no fixed numbers: their
//! targets are reserved atomically from the live process descriptor table
//! immediately before `posix_spawn` and remain reserved until spawn
//! completes. Internal protocol descriptors must never overwrite inherited
//! descriptors such as process-substitution `/dev/fd/N` handles.
//!
//! The parent writes only after `posix_spawn` returns, so no `pre_exec` fork
//! hazard exists; the child blocks on `read` until the payload arrives.
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

mod fd_layout;
mod helper;
mod protocol;

pub use fd_layout::{INTERNAL_FD_MIN, InternalFdLayout, InternalHelperFds};
pub use helper::run_internal_helper;
pub use protocol::{
    BuiltinExecRequest, InternalExecKind, InternalExecRequest, MAX_INTERNAL_EXEC_REQUEST,
    PROTOCOL_VERSION, PlanExecMode, PlanExecRequest, read_internal_request,
};

/// Resolve the binary a helper re-executes.
///
/// Production always re-executes `current_exe`. Under `cargo test` the
/// current image is the `deps/` test harness, which cannot serve the
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
    let in_deps = current
        .parent()
        .and_then(|dir| dir.file_name())
        .is_some_and(|name| name == "deps");
    if in_deps && let Some(bin_dir) = current.parent().and_then(|deps| deps.parent()) {
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
/// If the request payload cannot be fully delivered after a successful
/// spawn, the already-running helper is reaped synchronously and `Err` is
/// returned instead of a pid: the helper would otherwise exit on the
/// truncated request and surface as empty output, hiding the delivery
/// failure.
pub fn spawn_internal_helper(
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    request_bytes: &[u8],
    pgroup: Pid,
    status_write: Option<RawFd>,
) -> Result<Pid> {
    use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn};

    if request_bytes.len() > MAX_INTERNAL_EXEC_REQUEST {
        anyhow::bail!("internal exec request exceeds size limit");
    }
    let exe = helper_executable()?;

    let (req_read, req_write) =
        crate::process::io::cloexec_pipe().context("failed request pipe")?;
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

    // Inherit the process environment for the helper's fresh
    // `Environment::new()`; the snapshot then overwrites everything
    // authoritative. Entries with interior NUL cannot cross `execve` and are
    // skipped rather than failing the spawn.
    let envp: Vec<CString> = std::env::vars_os()
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

    let mut attr = PosixSpawnAttr::init().context("spawn attr")?;
    let mut spawn_flags = PosixSpawnFlags::empty();
    spawn_flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETPGROUP);
    spawn_flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF);
    spawn_flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK);
    attr.set_flags(spawn_flags).context("spawn flags")?;
    attr.set_pgroup(pgroup).context("spawn pgroup")?;
    // The helper must not inherit the shell's ignored dispositions: reset
    // the same job-control set the raw external-child path resets.
    let mut sigdefault = SigSet::empty();
    for sig in [
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTSTP,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
        Signal::SIGCHLD,
        Signal::SIGPIPE,
    ] {
        sigdefault.add(sig);
    }
    attr.set_sigdefault(&sigdefault)
        .context("spawn sigdefault")?;
    attr.set_sigmask(&SigSet::empty())
        .context("spawn sigmask")?;

    let child = match posix_spawn(&exe, &actions, &attr, &argv, &envp) {
        Ok(child) => child,
        Err(err) => {
            // Every temporary descriptor is `OwnedFd` (`req_read`,
            // `req_write`, and the `layout` reservations), so they drop
            // here with no manual cleanup branch.
            return Err(anyhow::anyhow!(err).context("failed to posix_spawn helper"));
        }
    };
    // The reservation served its purpose once spawn returned.
    drop(layout);
    // The child owns its read end via the file actions; closing ours lets it
    // see EOF after delivery.
    drop(req_read);

    // Deliver the payload, then close: the helper reads to EOF.
    if let Err(err) = fd_layout::write_all(req_write.as_fd(), request_bytes) {
        // The helper is already running: closing our write end lets it see
        // EOF (and exit on the truncated request) instead of hanging, then
        // reap it so no zombie is left behind.
        drop(req_write);
        let _ = crate::process::wait_pid_job(child, false);
        return Err(err);
    }
    drop(req_write);
    Ok(child)
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
    if dsh_builtin::background_builtin_mode(&process.name)
        == dsh_builtin::BackgroundBuiltinMode::ParentSessionRequired
    {
        let message = format!(
            "dogesh: {}: cannot run in background (needs the live shell session)\r\n",
            process.name
        );
        super::fork::write_process_stderr(process.stderr, message.as_bytes());
        anyhow::bail!("{} cannot run in background", process.name);
    }

    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&shell.environment.read()),
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: process.name.clone(),
            argv: process.argv.clone(),
            env_overrides: process.env_overrides.clone(),
        }),
    };
    let bytes = serde_json::to_vec(&request).context("encode internal request")?;
    // First pipeline stage starts a new group; later stages join the job's.
    let pgroup = ctx.pgid.unwrap_or(Pid::from_raw(0));
    let child = spawn_internal_helper(
        process.stdin,
        process.stdout,
        process.stderr,
        &bytes,
        pgroup,
        None,
    )?;
    process.pid = Some(child);
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
        }),
    };
    let bytes = serde_json::to_vec(&request).context("encode internal request")?;
    spawn_internal_helper(
        stdio.stdin,
        stdio.stdout,
        stdio.stderr,
        &bytes,
        pgroup,
        status_write,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An inherited auxiliary fd referenced as `/dev/fd/N` survives an
    /// internal helper spawn: the protocol's dynamically reserved targets
    /// must never overwrite it.
    #[test]
    fn plan_helper_preserves_inherited_aux_fd() {
        use std::io::{Read as _, Write as _};
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

        // Auxiliary pipe in the `<(...)` shape: non-CLOEXEC read end, so the
        // helper inherits it across `posix_spawn` like a consumer would.
        let mut aux = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(aux.as_mut_ptr()) }, 0, "pipe failed");
        // SAFETY: `pipe` succeeded, so both fds are owned exactly once.
        let aux_read = unsafe { OwnedFd::from_raw_fd(aux[0]) };
        let mut aux_write = unsafe { std::fs::File::from_raw_fd(aux[1]) };
        aux_write
            .write_all(b"FD-COLLISION-MARKER")
            .expect("write marker");
        drop(aux_write);
        let aux_number = aux_read.as_raw_fd();

        // `/bin/cat` exists on both Linux and macOS (unlike some other
        // coreutils, which live only under `/usr/bin` on macOS).
        let cat = "/bin/cat";
        let env_arc = crate::environment::Environment::new();
        let plan = crate::shell::parse::parse_execution_plan(
            &format!("{cat} /dev/fd/{aux_number}"),
            env_arc.clone(),
        )
        .expect("parse helper plan");
        let snapshot = ChildShellSnapshot::capture(&env_arc.read());

        let (cap_read, cap_write) = crate::process::io::cloexec_pipe().expect("capture pipe");
        let (status_read, status_write) = crate::process::io::cloexec_pipe().expect("status pipe");
        let null_in = std::fs::File::open("/dev/null").expect("open /dev/null");
        let null_err = std::fs::File::open("/dev/null").expect("open /dev/null");
        let producer = match spawn_plan_helper(
            &snapshot,
            &plan,
            PlanExecMode::CommandSubstitution,
            ChildStdio {
                stdin: null_in.as_raw_fd(),
                stdout: cap_write.as_raw_fd(),
                stderr: null_err.as_raw_fd(),
            },
            Pid::from_raw(0),
            Some(status_write.as_raw_fd()),
        ) {
            Ok(pid) => pid,
            // No silent skip: a missing helper binary means the test
            // invocation never built `dogesh` (run `cargo test -p doge-shell`
            // so the sibling binary exists). Passing vacuously would hide
            // the fd-collision regression this test guards.
            Err(err) => panic!("dogesh helper binary missing for re-exec test: {err:#}"),
        };
        drop(cap_write);
        drop(status_write);

        let mut output = Vec::new();
        std::fs::File::from(cap_read)
            .read_to_end(&mut output)
            .expect("read helper output");
        let _ = crate::process::wait_pid_job(producer, false);
        let mut verdict = [0u8; 1];
        let verdict_len = std::fs::File::from(status_read)
            .read(&mut verdict)
            .unwrap_or(0);
        assert_eq!(verdict_len, 1, "helper must report one status byte");
        assert_eq!(verdict[0], b'A', "helper plan must run to completion");
        assert_eq!(
            String::from_utf8_lossy(&output),
            "FD-COLLISION-MARKER",
            "aux fd /dev/fd/{aux_number} must survive the helper spawn"
        );
    }

    #[test]
    fn background_spawn_refuses_session_bound_builtins() {
        use nix::unistd::Pid;
        let env_arc = crate::environment::Environment::new();
        let mut shell = Shell::new(env_arc);
        let mut ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), false);
        let handler = dsh_builtin::get_handler("jobs").expect("jobs builtin");
        let mut process =
            BuiltinProcess::new_handler("jobs".to_string(), handler, vec!["jobs".to_string()]);
        let err = spawn_background_builtin(&mut ctx, &mut process, &mut shell)
            .expect_err("jobs must not re-exec");
        assert!(err.to_string().contains("cannot run in background"));
    }
}
