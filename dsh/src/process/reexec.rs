//! Versioned internal re-exec protocol: Rust code never runs after `fork`.
//!
//! Any child that must execute Rust (a background builtin today; isolated
//! subshell bodies in Phase 3) is spawned fresh instead of forked:
//!
//! ```text
//! parent shell
//!   → snapshot state (plain data, no live objects)
//!   → posix_spawn(current_exe, --__dsh-internal-exec-fd=3)
//!   → write JSON request to the pipe, close
//! child: fresh dogesh image
//!   → read request to EOF (size-capped)
//!   → fresh Tokio runtime, fresh Environment + Shell
//!   → run the already-materialized builtin argv, exit with its status
//! ```
//!
//! The request travels on a dedicated pipe duplicated to
//! [`INTERNAL_REQUEST_FD`], never on the command line (process-list and
//! secret exposure) or in the environment (size limits, inheritance). The
//! parent writes only after `posix_spawn` returns, so no `pre_exec` fork
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
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::os::fd::IntoRawFd as _;
use std::os::unix::io::RawFd;

/// Fixed fd the request pipe is duplicated to in the helper.
pub const INTERNAL_REQUEST_FD: RawFd = 3;
/// Fixed fd for the one-byte completion report (`b'A'` ran to completion,
/// `b'D'` denied by the safety policy, `b'E'` helper-internal error).
/// `CLOEXEC`: only the helper itself holds it, so the parent sees EOF as
/// soon as the helper exits or dies.
pub const INTERNAL_STATUS_FD: RawFd = 4;
/// Protocol version. Unknown versions fail closed.
pub const PROTOCOL_VERSION: u32 = 1;
/// Upper bound for one request; the child never does an unbounded
/// `read_to_end`. Oversized input is rejected with a non-zero exit.
pub const MAX_INTERNAL_EXEC_REQUEST: usize = 8 * 1024 * 1024;

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
    Builtin(BuiltinExecRequest),
    Plan(PlanExecRequest),
}

/// An isolated shell-execution body: `$(...)`, `<(...)`, or `( ... )`.
/// Structured and side-effect-free — the helper runs it through the same
/// gate → materialize → authorize → launch evaluator as the top level, never
/// by re-parsing source with `dogesh -c`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecRequest {
    pub plan: crate::shell::plan::ExecutionPlan,
    pub mode: PlanExecMode,
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
    /// `<(...)`: stdout is the producer stream; the parent reads `/dev/fd/N`
    /// and reaps the producer.
    ProcessSubstitution,
}

/// An already-materialized builtin invocation: no re-parse, no alias pass,
/// no runtime expansion, no second `SafetyGuard` round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinExecRequest {
    pub name: String,
    pub argv: Vec<String>,
    pub env_overrides: Vec<(String, String)>,
}

/// Read one request from `fd` to EOF, enforcing the size cap and version.
///
/// Takes ownership of `fd` and closes it on return, success or failure.
/// Malformed input (bad fd, oversized payload, invalid JSON, unknown
/// version) is a clean `Err`, never a panic — the helper request is a trust
/// boundary, and a user hand-invoking the hidden flag just gets a non-zero
/// exit, never privilege escalation.
pub fn read_internal_request(fd: RawFd) -> Result<InternalExecRequest> {
    if fd < 0 {
        anyhow::bail!("invalid internal exec fd");
    }
    // Raw `read(2)` on a borrowed fd: `File::from_raw_fd` would claim
    // ownership and abort on `close` for a bogus fd, but a user hand-invoking
    // the hidden flag deserves a clean error and a non-zero exit instead.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
        if n < 0 {
            let errno = std::io::Error::last_os_error();
            if errno.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            anyhow::bail!("failed to read internal exec request: {errno}");
        }
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if buf.len() > MAX_INTERNAL_EXEC_REQUEST {
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

/// Spawn a fresh `dogesh` helper carrying `request`, wired to the given
/// stdio, in process group `pgroup` (`Pid::from_raw(0)` = new group).
///
/// `request` must already be serialized and size-checked. `status_write`, when
/// given, is duplicated to [`INTERNAL_STATUS_FD`] for the helper's one-byte
/// completion report. Returns the helper pid; the caller owns reaping it
/// through the normal wait machinery, which maps signal deaths to
/// `128 + signal` without any duplicated logic.
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
    // `NixPath` is implemented for `PathBuf`, not `CString`.
    let exe_cstr = CString::new(exe.as_os_str().as_encoded_bytes())
        .context("dogesh path is not a valid CString")?;
    let flag = format!("--__dsh-internal-exec-fd={INTERNAL_REQUEST_FD}");
    let flag_cstr = CString::new(flag).expect("internal flag has no NUL");
    let argv = [exe_cstr.clone(), flag_cstr];

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

    let (req_read, req_write) =
        crate::process::io::cloexec_pipe().context("failed request pipe")?;
    let req_read_fd = req_read.into_raw_fd();
    let req_write_fd = req_write.into_raw_fd();

    let mut actions = PosixSpawnFileActions::init().context("spawn file actions")?;
    // Order matters: stdio first, then the request fd, then close the
    // original request read end (unless it already is fd 3).
    for (from, to) in [
        (stdin, libc::STDIN_FILENO),
        (stdout, libc::STDOUT_FILENO),
        (stderr, libc::STDERR_FILENO),
    ] {
        actions
            .add_dup2(from, to)
            .context("spawn stdio file action")?;
    }
    actions
        .add_dup2(req_read_fd, INTERNAL_REQUEST_FD)
        .context("spawn request-fd file action")?;
    if req_read_fd != INTERNAL_REQUEST_FD {
        actions
            .add_close(req_read_fd)
            .context("spawn request-fd close action")?;
    }
    if let Some(status_fd) = status_write {
        actions
            .add_dup2(status_fd, INTERNAL_STATUS_FD)
            .context("spawn status-fd file action")?;
        if status_fd != INTERNAL_STATUS_FD {
            actions
                .add_close(status_fd)
                .context("spawn status-fd close action")?;
        }
    }

    let mut attr = PosixSpawnAttr::init().context("spawn attr")?;
    let mut flags = PosixSpawnFlags::empty();
    flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETPGROUP);
    flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF);
    flags.insert(PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK);
    attr.set_flags(flags).context("spawn flags")?;
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

    let child =
        posix_spawn(&exe, &actions, &attr, &argv, &envp).context("posix_spawn dogesh helper")?;
    // The child inherited its own read end via the file actions; close ours.
    unsafe { libc::close(req_read_fd) };

    // Deliver the payload, then close: the helper reads to EOF.
    write_all(req_write_fd, request_bytes);
    unsafe { libc::close(req_write_fd) };
    Ok(child)
}

fn write_all(fd: RawFd, mut bytes: &[u8]) {
    use std::os::fd::BorrowedFd;
    while !bytes.is_empty() {
        let fd_ref = unsafe { BorrowedFd::borrow_raw(fd) };
        match nix::unistd::write(fd_ref, bytes) {
            Ok(0) => break,
            Ok(n) => bytes = &bytes[n..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }
    }
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

/// Run one helper request to completion inside the fresh process.
///
/// Called before any interactive startup (no `config.lisp`, no history, no
/// MCP, no notebook, no lifecycle activation): the helper is an execution
/// detail of the parent session, not a new interactive agent. Returns the
/// process exit code.
pub async fn run_internal_helper(exec_fd: RawFd) -> std::process::ExitCode {
    match run_internal_helper_inner(exec_fd).await {
        Ok(code) => std::process::ExitCode::from(code),
        Err(err) => {
            eprintln!("dogesh: internal exec failed: {err:#}");
            std::process::ExitCode::from(1)
        }
    }
}

async fn run_internal_helper_inner(exec_fd: RawFd) -> Result<u8> {
    let request = read_internal_request(exec_fd)?;
    // The status fd arrives non-CLOEXEC (dup2 clears the flag on its target):
    // mark it close-on-exec here so nested external children and sub-helpers
    // cannot hold it open past their own `execve`, which would hide the
    // parent's EOF. Absent for builtins — ignore that failure.
    unsafe {
        use std::os::fd::BorrowedFd;
        let status_fd = BorrowedFd::borrow_raw(INTERNAL_STATUS_FD);
        if let Ok(flags) = nix::fcntl::fcntl(status_fd, nix::fcntl::FcntlArg::F_GETFD) {
            let mut bits = nix::fcntl::FdFlag::from_bits_retain(flags);
            bits.insert(nix::fcntl::FdFlag::FD_CLOEXEC);
            let _ = nix::fcntl::fcntl(status_fd, nix::fcntl::FcntlArg::F_SETFD(bits));
        }
    }

    let env_arc = crate::environment::Environment::new();
    {
        let mut env = env_arc.write();
        request.snapshot.apply_to(&mut env);
    }
    // Follow the snapshot cwd explicitly, not spawn inheritance alone.
    if let Err(err) = std::env::set_current_dir(&request.snapshot.cwd) {
        eprintln!(
            "dogesh: internal exec: cannot chdir to {}: {err}",
            request.snapshot.cwd.display()
        );
        return Ok(1);
    }
    let mut shell = Shell::new(env_arc);

    match &request.kind {
        InternalExecKind::Builtin(builtin) => run_helper_builtin(&mut shell, builtin).await,
        InternalExecKind::Plan(plan_request) => run_helper_plan(&mut shell, plan_request).await,
    }
}

async fn run_helper_builtin(shell: &mut Shell, builtin: &BuiltinExecRequest) -> Result<u8> {
    // Command-scoped `NAME=value` prefixes: apply as exported vars in the
    // helper only; the helper exits right after, so nothing leaks back.
    for (key, value) in &builtin.env_overrides {
        shell
            .environment
            .write()
            .set_shell_var(key.clone(), value.clone());
        shell
            .environment
            .write()
            .variable_state
            .exported_vars
            .insert(key.clone());
    }

    let Some(handler) = dsh_builtin::get_handler(&builtin.name) else {
        eprintln!("dogesh: internal exec: unknown builtin: {}", builtin.name);
        return Ok(2);
    };
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.foreground = false;
    ctx.interactive = false;
    ctx.save_history = false;
    // Fresh Tokio runtime via the caller's `block_on`: async handlers run
    // their real async implementation, not the sync fallback.
    let status = handler.execute(&ctx, builtin.argv.clone(), shell).await;
    Ok(exit_code_of(status))
}

async fn run_helper_plan(shell: &mut Shell, plan_request: &PlanExecRequest) -> Result<u8> {
    // The helper owns no terminal UI: confirmation prompts must never touch
    // stdout (it is data in capture mode). `helper_confirm` asks on
    // `/dev/tty` and fails closed when there is none.
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.foreground = false;
    ctx.interactive = false;
    ctx.save_history = false;
    // The helper's stdout is already the final destination (terminal, or the
    // parent's capture pipe). Marking it as caller-captured suppresses the
    // non-interactive auto-capture: without this every nested external would
    // grow a capture pipe plus an `OutputMonitor` that re-emits its output
    // with a `\r\n` display prefix — byte pollution inside `$(...)`.
    // This mirrors the old in-process substitution loop, which set
    // `captured_out` for the same reason.
    ctx.captured_out = Some(libc::STDOUT_FILENO);
    // Process-substitution producers lead their own process group (spawned
    // with a fresh pgid): preset it so every nested spawn joins the group
    // and the parent's group-kill reaper reaches the whole tree, including
    // grandchildren that outlive the helper itself.
    if plan_request.mode == PlanExecMode::ProcessSubstitution {
        ctx.pgid = Some(shell.pid);
    }
    let outcome =
        crate::shell::eval::evaluate_plan(shell, &mut ctx, &plan_request.plan, helper_confirm)
            .await;
    match outcome {
        Ok(code) => {
            report_status_byte(b'A');
            Ok(code.clamp(0, 255) as u8)
        }
        Err(err) if crate::shell::authorize::is_authorization_cancelled(&err) => {
            // A nested denial must abort the whole outer chain, not surface
            // as empty output: report it on the status fd so the parent maps
            // it back to `AuthorizationCancelled`. Exit 130 either way.
            report_status_byte(b'D');
            Ok(130)
        }
        Err(err) => {
            eprintln!("dogesh: internal exec plan failed: {err:#}");
            report_status_byte(b'E');
            Ok(1)
        }
    }
}

/// Best-effort one-byte completion report. Failures (parent already gone,
/// no status fd for builtins) are ignored: the exit code still carries the
/// primary signal.
fn report_status_byte(byte: u8) {
    let buf = [byte];
    unsafe {
        libc::write(
            INTERNAL_STATUS_FD,
            buf.as_ptr() as *const libc::c_void,
            buf.len(),
        );
    }
}

/// Confirmation prompter for re-exec helpers.
///
/// The interactive `confirm_action` draws on stdout with keypress input —
/// unusable here because stdout is captured data and the helper shares the
/// terminal with a blocked parent. This asks one line on `/dev/tty`
/// instead, and answers `No` (fail closed) when no terminal is available, so
/// `$(rm -rf ...)` in a pipeline can never self-approve.
///
/// An `AlwaysAllow` answer only touches the helper's own allowlist, which
/// dies with it; nothing is persisted into the parent session.
fn helper_confirm(message: &str) -> Result<crate::repl::confirmation::ConfirmationAction> {
    use crate::repl::confirmation::ConfirmationAction;
    use std::io::{BufRead, Write};

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty");
    let Ok(mut tty) = tty else {
        return Ok(ConfirmationAction::No);
    };
    if writeln!(
        tty,
        "SAFETY GUARD (background task): {message}\r\nProceed? [y/N/a(Always for this task)]: "
    )
    .is_err()
    {
        return Ok(ConfirmationAction::No);
    }
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(tty.try_clone().map_err(|err| anyhow::anyhow!(err))?);
    if reader.read_line(&mut line).is_err() {
        return Ok(ConfirmationAction::No);
    }
    Ok(match line.trim().to_lowercase().as_str() {
        "y" | "yes" => ConfirmationAction::Yes,
        "a" | "always" => ConfirmationAction::AlwaysAllow,
        _ => ConfirmationAction::No,
    })
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

fn exit_code_of(status: dsh_types::ExitStatus) -> u8 {
    use dsh_types::ExitStatus;
    match status {
        ExitStatus::ExitedWith(code) if code >= 0 => code.clamp(0, 255) as u8,
        ExitStatus::ExitedWith(_) => 1,
        // A detached pid or control-flow marker inside a one-shot helper has
        // nothing left to report to: success.
        ExitStatus::Running(_) | ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd as _;

    #[test]
    fn request_rejects_unknown_version() {
        let env_arc = crate::environment::Environment::new();
        let snapshot = ChildShellSnapshot::capture(&env_arc.read());
        let request = InternalExecRequest {
            version: PROTOCOL_VERSION + 1,
            snapshot,
            kind: InternalExecKind::Builtin(BuiltinExecRequest {
                name: "echo".to_string(),
                argv: vec!["echo".to_string()],
                env_overrides: vec![],
            }),
        };
        let bytes = serde_json::to_vec(&request).expect("encode");
        // Feed through a real pipe so the size-cap and EOF logic run.
        let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
        write.write_all(&bytes).expect("write");
        drop(write);
        let read_fd = read.into_raw_fd();
        // `read_internal_request` owns and closes the fd; no second close.
        let err = read_internal_request(read_fd).expect_err("unknown version must fail");
        assert!(
            err.to_string()
                .contains("unsupported internal exec version")
        );
    }

    #[test]
    fn request_rejects_garbage() {
        let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
        write.write_all(b"not json").expect("write");
        drop(write);
        let read_fd = read.into_raw_fd();
        // Owned (and closed) by `read_internal_request`.
        assert!(read_internal_request(read_fd).is_err());
    }

    fn pipe_with_request(request: &InternalExecRequest) -> RawFd {
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let bytes = serde_json::to_vec(request).expect("encode");
        let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
        let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
        write.write_all(&bytes).expect("write");
        drop(write);
        read.into_raw_fd()
    }

    fn builtin_request(name: &str, argv: Vec<String>) -> InternalExecRequest {
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

    #[tokio::test]
    async fn helper_runs_builtin_and_reports_status() {
        // `cd` to a missing directory: the async handler runs for real in
        // the helper and its non-zero status becomes the exit code.
        let request = builtin_request("cd", vec!["cd".to_string(), "/dsh-no-such-dir".to_string()]);
        let fd = pipe_with_request(&request);
        let code = run_internal_helper_inner(fd).await.expect("helper runs");
        assert_eq!(code, 1);
    }

    #[tokio::test]
    async fn helper_rejects_unknown_builtin_without_panic() {
        let request = builtin_request(
            "dsh-no-such-builtin",
            vec!["dsh-no-such-builtin".to_string()],
        );
        let fd = pipe_with_request(&request);
        let code = run_internal_helper_inner(fd).await.expect("helper runs");
        assert_ne!(code, 0);
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
        let err =
            super::super::reexec::spawn_background_builtin(&mut ctx, &mut process, &mut shell)
                .expect_err("jobs must not re-exec");
        assert!(err.to_string().contains("cannot run in background"));
    }
}
