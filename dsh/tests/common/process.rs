//! Explicit process handle for a `dogesh` test child.
//!
//! The legacy helpers in [`super`] (`run_dsh`, `run_command`, ...) serialize
//! every child behind a global lock and panic on timeout. That behavior is
//! preserved untouched. This module adds the explicit counterpart for tests
//! that need the child itself:
//!
//! - [`spawn_dsh_unlocked`]: no global lock; only dedicated concurrency
//!   tests may use it, so ordinary tests can never accidentally go parallel.
//! - [`DshTestProcess`]: RAII handle (stdin writes, bounded wait,
//!   process-group cleanup, opt-in group-drain assertion).
//!
//! Ownership rule mirrored from the shell itself: every live helper has
//! exactly one logical owner, and a group is never left behind. `Drop`
//! best-effort kills the group but never panics; failures surface through
//! [`DshTestProcess::wait`] / [`DshTestProcess::assert_group_drained`].

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tempfile::TempDir;
use wait_timeout::ChildExt;

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

    /// Wait up to `timeout`, then collect output.
    ///
    /// On timeout the group is `SIGKILL`ed, reaped, and a
    /// [`WaitError::TimedOut`] carrying partial output is returned so the
    /// caller can record it as a case failure instead of panicking mid-suite.
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
        let output = {
            let child = self.child.as_mut().expect("DshTestProcess already waited");
            match child
                .wait_timeout(timeout)
                .expect("failed while waiting for dsh")
            {
                Some(_) => {
                    let child = self.child.take().expect("child present");
                    child
                        .wait_with_output()
                        .map_err(|err| WaitError::Io(err.to_string()))?
                }
                None => {
                    self.kill_group();
                    let child = self.child.take().expect("child present");
                    let output = child.wait_with_output().unwrap_or_else(|_| Output {
                        status: std::os::unix::process::ExitStatusExt::from_raw(9 << 8),
                        stdout: Vec::new(),
                        stderr: format!("dsh did not exit within {timeout:?}").into_bytes(),
                    });
                    return Err(WaitError::TimedOut(output));
                }
            }
        };
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
            WaitError::TimedOut(output) => format!(
                "dsh did not exit within {timeout:?}\nstdout:\n{}\nstderr:\n{}",
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
    TimedOut(Output),
    Io(String),
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
    let temp = TempDir::new().expect("failed to create isolated dsh test directory");
    let workdir = temp.path().join("work");
    std::fs::create_dir_all(&workdir).expect("create isolated contract cwd");
    let mut child = Command::new(env!("CARGO_BIN_EXE_dogesh"))
        .args(args)
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
