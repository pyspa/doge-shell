//! Explicit process handle for a `dogesh` test child.
//!
//! The legacy helpers in [`super`] (`run_dsh`, `run_command`, ...) serialize
//! every child behind a global lock and panic on timeout. That behavior is
//! preserved untouched. This module adds the explicit counterpart for tests
//! that need the child itself:
//!
//! - [`spawn_dsh_unlocked`]: no global lock; only dedicated concurrency
//!   tests may use it, so ordinary tests can never accidentally go parallel.
//! - [`spawn_dsh_unlocked_with_nofile_limit`]: same as above, but only the
//!   child's own `RLIMIT_NOFILE` is lowered (the parent test process keeps
//!   its limits).
//! - [`DshTestProcess`]: RAII handle (stdin writes, total-deadline
//!   process + output collection, process-group cleanup, opt-in
//!   group-drain assertion).
//!
//! Ownership rule mirrored from the shell itself: every live helper has
//! exactly one logical owner, and a group is never left behind. `Drop`
//! best-effort kills the group but never panics; failures surface through
//! [`DshTestProcess::wait`] / [`DshTestProcess::assert_group_drained`].
//!
//! Total-deadline rule: a case timeout bounds primary process exit **and**
//! capture-pipe EOF as one absolute deadline. Primary exit alone never
//! completes a case; legitimate descendant-held output is collected until
//! EOF or the deadline. Past the deadline the harness kills the owned
//! shell process group, performs one ready-now partial drain, closes its
//! capture readers, and reports `Timeout` — it never waits indefinitely
//! for EOF.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tempfile::TempDir;

/// Default per-case bound for a contract child.
pub const DEFAULT_CASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Deterministic environment for contract children.
///
/// Each child gets an isolated `HOME`/XDG tree plus locale/terminal pins so
/// output never depends on the developer's machine. `PATH` is inherited
/// from the host (contracts resolve externals through it); color is pinned
/// off via `TERM=dumb` + `NO_COLOR`.
pub fn contract_env(temp: &TempDir) -> Vec<(String, String)> {
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    for dir in [&home, &state, &data, &config] {
        std::fs::create_dir_all(dir).expect("create isolated contract dir");
    }
    vec![
        ("HOME".to_string(), home.to_string_lossy().to_string()),
        (
            "XDG_STATE_HOME".to_string(),
            state.to_string_lossy().to_string(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            data.to_string_lossy().to_string(),
        ),
        (
            "XDG_CONFIG_HOME".to_string(),
            config.to_string_lossy().to_string(),
        ),
        ("LC_ALL".to_string(), "C".to_string()),
        ("LANG".to_string(), "C".to_string()),
        ("TERM".to_string(), "dumb".to_string()),
        ("NO_COLOR".to_string(), "1".to_string()),
    ]
}

/// A live `dogesh` child plus the isolated dirs it runs in.
///
/// The temp dir is owned here so the child's cwd/HOME/XDG tree cannot vanish
/// while it is still running. `Drop` kills a still-live group (best effort,
///
/// never panics); use [`DshTestProcess::wait`] for the asserting path.
pub struct DshTestProcess {
    child: Option<Child>,
    pgid: Pid,
    temp: Option<TempDir>,
    workdir: Option<PathBuf>,
}

impl DshTestProcess {
    /// Process group of the child. Usable for group-drain assertions.
    pub fn pgid(&self) -> Pid {
        self.pgid
    }

    /// Isolated working directory (cwd/HOME root) of this child.
    pub fn workdir(&self) -> &Path {
        self.workdir.as_deref().expect("DshTestProcess dirs taken")
    }

    /// Write to the child's stdin without closing it.
    pub fn write_stdin(&mut self, input: &str) -> std::io::Result<()> {
        let stdin = self
            .child
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "dsh stdin is closed")
            })?;
        stdin.write_all(input.as_bytes())?;
        stdin.flush()
    }

    /// Close the child's stdin (sends EOF).
    pub fn close_stdin(&mut self) {
        if let Some(child) = self.child.as_mut() {
            drop(child.stdin.take());
        }
    }

    /// Best-effort `SIGKILL` of the whole child group.
    pub fn kill_group(&mut self) {
        if self.child.is_some() {
            let _ = killpg(self.pgid, Signal::SIGKILL);
        }
    }

    /// Wait up to `timeout` for process exit **and** capture-pipe EOF.
    ///
    /// The timeout is one absolute deadline covering primary exit plus
    /// stdout/stderr EOF. On deadline the group is `SIGKILL`ed, reaped, and a
    /// [`WaitError::TimedOut`] carrying partial output plus the timeout
    /// phase is returned so the caller can record it as a case failure
    /// instead of panicking mid-suite.
    ///
    /// The isolated dirs are dropped with `self`; callers asserting on
    /// side-effect files must use [`DshTestProcess::wait_keep_dirs`] so the
    /// files survive until after the assertions.
    pub fn wait(self, timeout: Duration) -> Result<Output, WaitError> {
        self.wait_keep_dirs(timeout).map(|waited| waited.output)
    }

    /// [`DshTestProcess::wait`], but the isolated temp dirs and workdir are
    /// returned alive for post-exit file assertions.
    pub fn wait_keep_dirs(mut self, timeout: Duration) -> Result<WaitedProcess, WaitError> {
        let child = self.child.take().expect("DshTestProcess already waited");
        let output = wait_child_with_output_deadline(child, self.pgid, timeout)?;
        // `Option::take` moves out cleanly despite the `Drop` impl; the
        // remainder (`child: None`) drops as a no-op.
        let temp = self.temp.take().expect("dirs present");
        let workdir = self.workdir.take().expect("dirs present");
        Ok(WaitedProcess {
            output,
            _temp: temp,
            workdir,
        })
    }

    /// Bounded poll until no process remains in the child's group.
    ///
    /// Success is `ESRCH` from `killpg(pgid, 0)`. On failure the group is
    /// `SIGKILL`ed and reaped so CI never inherits strays, then an error
    /// describing the leak is returned.
    ///
    /// Assumption: the poll starts immediately after the shell's own exit
    /// and lasts at most 3s, so a passing result cannot observe an
    /// unrelated group that recycled the pgid — recycling within that
    /// window would require pid wraparound on a busy host. A failure only
    /// ever escalates to `SIGKILL`, never to a green assertion.
    pub fn assert_group_drained(self, timeout: Duration) -> Result<Output, String> {
        let pgid = self.pgid;
        let output = self.wait(timeout).map_err(|err| match err {
            WaitError::TimedOut {
                output,
                phase,
                stdout_eof,
                stderr_eof,
            } => format!(
                "dsh did not complete within {timeout:?} (phase: {}, stdout eof: {stdout_eof}, stderr eof: {stderr_eof})\nstdout:\n{}\nstderr:\n{}",
                phase.as_str(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            WaitError::Io(err) => format!("failed to collect dsh output: {err}"),
        })?;
        // The shell is gone; descendants must follow within a bounded poll.
        // `killpg(pgid, None)` is the zero-signal existence probe.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match killpg(pgid, None) {
                Err(nix::errno::Errno::ESRCH) => return Ok(output),
                _ => {
                    if Instant::now() >= deadline {
                        let _ = killpg(pgid, Signal::SIGKILL);
                        std::thread::sleep(Duration::from_millis(200));
                        return Err(format!(
                            "process group {pgid} still alive 3s after shell exit (status: {})\nstdout:\n{}\nstderr:\n{}",
                            output.status,
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for DshTestProcess {
    fn drop(&mut self) {
        // Cleanup only, never panics: failures belong to wait()/assert paths.
        if self.child.is_some() {
            let _ = killpg(self.pgid, Signal::SIGKILL);
            if let Some(mut child) = self.child.take() {
                let _ = child.wait();
            }
        }
    }
}

/// How a bounded wait can fail without panicking.
#[derive(Debug)]
pub enum WaitError {
    TimedOut {
        output: Output,
        phase: WaitTimeoutPhase,
        stdout_eof: bool,
        stderr_eof: bool,
    },
    Io(String),
}

/// Which half of the total deadline ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitTimeoutPhase {
    /// The primary child was still running at the deadline.
    ProcessExit,
    /// The primary child had exited, but stdout/stderr had not reached EOF.
    OutputEof,
}

impl WaitTimeoutPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            WaitTimeoutPhase::ProcessExit => "process-exit",
            WaitTimeoutPhase::OutputEof => "output-eof",
        }
    }
}

/// Per-iteration fairness budget for one capture stream.
///
/// A continuous producer must not monopolize the collection loop: after
/// this many bytes the loop returns to `try_wait()` and the deadline check.
/// Mirrors the production running-drain fairness policy; kept as a harness
/// own constant because the goals differ.
const CAPTURE_DRAIN_BUDGET_BYTES: usize = 256 * 1024;

/// Wake-up slice for the capture loop.
///
/// The actual poll timeout is `min(deadline - now, CAPTURE_POLL_SLICE)` so
/// pipe silence never delays primary-exit observation beyond ~25ms.
const CAPTURE_POLL_SLICE: Duration = Duration::from_millis(25);

/// Result of one nonblocking pipe drain.
enum PipeDrain {
    Open,
    Eof,
}

/// Collect `child` output under a single absolute deadline.
///
/// Success requires primary status **and** stdout EOF **and** stderr EOF
/// before the deadline. `Child::wait_with_output()` is never used: it
/// cannot bound EOF collection. Reader threads are not used either: a
/// blocked `read_to_end()` cannot be cancelled safely.
///
/// Both capture pipes are switched to `O_NONBLOCK` (the same `fcntl`
/// method as production `OutputMonitor`), drained fairly with a per-stream
/// byte budget, and woken with a short `libc::poll()`. `revents` never
/// decides EOF; only a direct `read()` returning `Ok(0)` (EOF) or
/// `WouldBlock` (still open) is authoritative.
///
/// On deadline the owned shell process group is `SIGKILL`ed (direct
/// `child.kill()` fallback), the primary is reaped, one ready-now drain
/// keeps partial output, the readers are dropped, and `TimedOut` is
/// returned. EOF is never waited for after the deadline. No process
/// enumeration and no `waitpid(-1)`: unrelated groups are untouched.
pub(crate) fn wait_child_with_output_deadline(
    mut child: Child,
    pgid: Pid,
    timeout: Duration,
) -> Result<Output, WaitError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("test timeout overflow");

    // Like `wait_with_output()`: closing stdin signals input EOF up front.
    drop(child.stdin.take());

    let mut stdout: Option<ChildStdout> = child.stdout.take();
    let mut stderr: Option<ChildStderr> = child.stderr.take();
    if let Some(stream) = stdout.as_ref() {
        let current =
            fcntl(stream, FcntlArg::F_GETFL).map_err(|err| WaitError::Io(err.to_string()))?;
        let flags = OFlag::from_bits_truncate(current) | OFlag::O_NONBLOCK;
        fcntl(stream, FcntlArg::F_SETFL(flags)).map_err(|err| WaitError::Io(err.to_string()))?;
    }
    if let Some(stream) = stderr.as_ref() {
        let current =
            fcntl(stream, FcntlArg::F_GETFL).map_err(|err| WaitError::Io(err.to_string()))?;
        let flags = OFlag::from_bits_truncate(current) | OFlag::O_NONBLOCK;
        fcntl(stream, FcntlArg::F_SETFL(flags)).map_err(|err| WaitError::Io(err.to_string()))?;
    }

    let mut collected_stdout = Vec::new();
    let mut collected_stderr = Vec::new();
    let mut status: Option<ExitStatus> = None;
    let mut stdout_eof = stdout.is_none();
    let mut stderr_eof = stderr.is_none();

    loop {
        if !stdout_eof {
            let reader = stdout.as_mut().expect("stdout present");
            match drain_ready(reader, &mut collected_stdout, CAPTURE_DRAIN_BUDGET_BYTES)
                .map_err(WaitError::Io)?
            {
                PipeDrain::Eof => stdout_eof = true,
                PipeDrain::Open => {}
            }
        }
        if !stderr_eof {
            let reader = stderr.as_mut().expect("stderr present");
            match drain_ready(reader, &mut collected_stderr, CAPTURE_DRAIN_BUDGET_BYTES)
                .map_err(WaitError::Io)?
            {
                PipeDrain::Eof => stderr_eof = true,
                PipeDrain::Open => {}
            }
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|err| WaitError::Io(err.to_string()))?;
        }
        if let Some(exit) = status
            && stdout_eof
            && stderr_eof
        {
            return Ok(Output {
                status: exit,
                stdout: collected_stdout,
                stderr: collected_stderr,
            });
        }
        if Instant::now() >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if stdout_eof && stderr_eof {
            // Pipes are done but the primary lives on: no fd to poll.
            std::thread::sleep(remaining.min(CAPTURE_POLL_SLICE));
            continue;
        }
        poll_captures(
            stdout.as_ref().map(AsRawFd::as_raw_fd),
            stderr.as_ref().map(AsRawFd::as_raw_fd),
            stdout_eof,
            stderr_eof,
            remaining.min(CAPTURE_POLL_SLICE),
        )?;
    }

    // Deadline path: never wait for EOF again.
    if status.is_none() {
        status = child
            .try_wait()
            .map_err(|err| WaitError::Io(err.to_string()))?;
    }
    let phase = if status.is_some() {
        WaitTimeoutPhase::OutputEof
    } else {
        WaitTimeoutPhase::ProcessExit
    };
    if status.is_none() {
        match killpg(pgid, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(_) => {
                let _ = child.kill();
            }
        }
        let reaped = child.wait().map_err(|err| WaitError::Io(err.to_string()))?;
        status = Some(reaped);
    }
    // One ready-now drain keeps diagnostics; EOF is not awaited.
    if !stdout_eof
        && let Some(reader) = stdout.as_mut()
        && let Ok(PipeDrain::Eof) =
            drain_ready(reader, &mut collected_stdout, CAPTURE_DRAIN_BUDGET_BYTES)
    {
        stdout_eof = true;
    }
    if !stderr_eof
        && let Some(reader) = stderr.as_mut()
        && let Ok(PipeDrain::Eof) =
            drain_ready(reader, &mut collected_stderr, CAPTURE_DRAIN_BUDGET_BYTES)
    {
        stderr_eof = true;
    }
    drop(stdout);
    drop(stderr);
    let output = Output {
        status: status.expect("primary reaped on deadline"),
        stdout: collected_stdout,
        stderr: collected_stderr,
    };
    Err(WaitError::TimedOut {
        output,
        phase,
        stdout_eof,
        stderr_eof,
    })
}

/// Drain what is ready now without blocking.
///
/// `Ok(0)` is EOF (authoritative), `WouldBlock` means still open,
/// `Interrupted` retries. The byte budget guarantees a return to the
/// `try_wait()` / deadline check even under continuous output.
fn drain_ready<R>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    byte_budget: usize,
) -> Result<PipeDrain, String>
where
    R: Read + AsFd,
{
    let mut bytes_read = 0usize;
    let mut chunk = [0u8; 8192];
    loop {
        if bytes_read >= byte_budget {
            return Ok(PipeDrain::Open);
        }
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(PipeDrain::Eof),
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                bytes_read += n;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(PipeDrain::Open);
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// Wake-up-only poll for the still-open capture pipes.
///
/// `revents` never decides EOF; the caller always follows up with a direct
/// `read()`. `EINTR` retries via the next loop iteration.
fn poll_captures(
    stdout_fd: Option<std::os::fd::RawFd>,
    stderr_fd: Option<std::os::fd::RawFd>,
    stdout_eof: bool,
    stderr_eof: bool,
    timeout: Duration,
) -> Result<(), WaitError> {
    let mut fds = Vec::with_capacity(2);
    if !stdout_eof && let Some(fd) = stdout_fd {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        });
    }
    if !stderr_eof && let Some(fd) = stderr_fd {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        });
    }
    if fds.is_empty() {
        return Ok(());
    }
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: `fds` is a live mutable slice of `pollfd` for the call.
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if result >= 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::Interrupted {
        return Ok(());
    }
    Err(WaitError::Io(err.to_string()))
}

/// A reaped child whose isolated dirs are still alive for file assertions.
pub struct WaitedProcess {
    pub output: Output,
    _temp: TempDir,
    pub workdir: PathBuf,
}

/// Spawn a `dogesh` child with **no** global serialization.
///
/// Only dedicated concurrency tests may call this directly. Ordinary tests
/// must go through the serial wrappers in [`super`] so they stay serialized
/// inside the test binary.
pub fn spawn_dsh_unlocked<I, S>(args: I, input: Option<&str>) -> DshTestProcess
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let command = {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dogesh"));
        command.args(args);
        command
    };
    spawn_child(command, input)
}

/// Spawn a `dogesh` child with only its own `RLIMIT_NOFILE` lowered.
///
/// The parent `cargo test` / nextest process keeps its limits: the bound is
/// applied inside a `/bin/sh` wrapper (`ulimit -n "$1"; shift; exec "$@"`)
/// that `exec`s dogesh, so the child PID/PGID ownership, isolated
/// cwd/HOME/XDG, capture pipes, `process_group(0)`, and bounded wait stay
/// identical to [`spawn_dsh_unlocked`]. Dogesh and script travel as
/// positional arguments, never interpolated into the wrapper string.
///
/// A wrapper `ulimit` failure exits 97 so a platform that rejects the limit
/// fails loudly instead of running unbounded.
pub fn spawn_dsh_unlocked_with_nofile_limit<I, S>(
    args: I,
    input: Option<&str>,
    nofile_limit: u64,
) -> DshTestProcess
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("ulimit -n \"$1\" || exit 97; shift; exec \"$@\"")
        .arg("dogesh-rlimit-wrapper")
        .arg(nofile_limit.to_string())
        .arg(env!("CARGO_BIN_EXE_dogesh"))
        .args(args);
    spawn_child(command, input)
}

/// Finish spawning `command` exactly the way [`spawn_dsh_unlocked`] does:
/// isolated dirs, deterministic env, captured stdio, own process group,
/// bounded-wait RAII handle.
fn spawn_child(mut command: Command, input: Option<&str>) -> DshTestProcess {
    let temp = TempDir::new().expect("failed to create isolated dsh test directory");
    let workdir = temp.path().join("work");
    std::fs::create_dir_all(&workdir).expect("create isolated contract cwd");
    let mut child = command
        .current_dir(&workdir)
        .envs(contract_env(&temp))
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("failed to spawn dsh");
    let pgid = Pid::from_raw(child.id() as i32);

    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("failed to open dsh stdin");
        stdin
            .write_all(input.as_bytes())
            .expect("failed to write dsh stdin");
        // `stdin` drops here: EOF, matching the legacy runner.
    }

    DshTestProcess {
        child: Some(child),
        pgid,
        temp: Some(temp),
        workdir: Some(workdir),
    }
}

/// Absolute path to an external `sh` for helper scripts.
///
/// `/bin/sh` ships on both supported platforms (POSIX on Linux, present on
/// macOS), so unlike `true`/`false` no per-platform candidate list is
/// needed. Contracts still spell it `{{SH}}`, never hardcoded.
pub fn sh_path() -> &'static str {
    "/bin/sh"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_env_pins_deterministic_keys() {
        let temp = TempDir::new().expect("tempdir");
        let env = contract_env(&temp);
        let get = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("missing {key}"))
        };
        assert_eq!(get("LC_ALL"), "C");
        assert_eq!(get("TERM"), "dumb");
        assert!(get("HOME").starts_with(temp.path().to_str().unwrap()));
        assert!(!env.iter().any(|(k, _)| k == "PATH"));
    }
}
