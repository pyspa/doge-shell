//! Shared integration-test runner for `dogesh` children.
//!
//! The historical helpers below keep their exact behavior: isolated XDG
//! dirs, stdin isolation, captured output, child process group, bounded
//! wait with group `SIGKILL` on timeout, and serialization behind a global
//! lock. New structure lives in modules:
//!
//! - [`process`]: [`process::DshTestProcess`] RAII handle plus the explicit
//!   unlocked spawner (concurrency tests only).
//! - [`contract`]: declarative TOML contract harness (Layer 1).

#![allow(dead_code)]

pub mod contract;
pub mod process;

use nix::unistd::Pid;
use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tempfile::TempDir;

fn child_process_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hold the serial child-execution lock for a scope larger than one spawn
/// (e.g. a whole contract file). Ordinary tests never need this; they use
/// the serial helpers which lock per call.
///
/// While the guard is held, spawn only via [`process::spawn_dsh_unlocked`]:
/// the serial helpers (`run_dsh`, `run_command`, ...) lock the same mutex
/// and would self-deadlock.
pub fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    child_process_lock()
}

pub fn run_dsh<I, S>(args: I, timeout: Duration) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_dsh_with_input(args, None, timeout)
}

pub fn run_dsh_with_input<I, S>(args: I, input: Option<&str>, timeout: Duration) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let _guard = child_process_lock();
    let temp = TempDir::new().expect("failed to create isolated dsh test directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_dogesh"))
        .args(args)
        .env("XDG_STATE_HOME", temp.path())
        .env("XDG_DATA_HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path())
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

    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("failed to open dsh stdin");
        stdin
            .write_all(input.as_bytes())
            .expect("failed to write dsh stdin");
    }

    let pgid = Pid::from_raw(child.id() as i32);
    match process::wait_child_with_output_deadline(child, pgid, timeout) {
        Ok(output) => output,
        Err(process::WaitError::TimedOut {
            output,
            phase,
            stdout_eof,
            stderr_eof,
        }) => panic!(
            "dsh did not complete within {timeout:?} (phase: {}, stdout eof: {stdout_eof}, stderr eof: {stderr_eof})\nstdout:\n{}\nstderr:\n{}",
            phase.as_str(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(process::WaitError::Io(err)) => {
            panic!("failed to collect dsh output: {err}");
        }
    }
}

/// Absolute path to an external `true`.
///
/// The tests spell these out absolutely so the shell resolves a real external
/// command instead of a builtin, but the location differs by platform: macOS
/// ships them only under `/usr/bin`, while some Linux layouts have them only
/// under `/bin`.
pub fn true_path() -> &'static str {
    first_existing(&["/bin/true", "/usr/bin/true"])
}

/// Absolute path to an external `false`. See [`true_path`].
pub fn false_path() -> &'static str {
    first_existing(&["/bin/false", "/usr/bin/false"])
}

/// Absolute path to an external `tr`. See [`true_path`].
pub fn tr_path() -> &'static str {
    first_existing(&["/bin/tr", "/usr/bin/tr"])
}

/// Absolute path to an external `yes`. See [`true_path`].
pub fn yes_path() -> &'static str {
    first_existing(&["/bin/yes", "/usr/bin/yes"])
}

/// Absolute path to an external `head`. See [`true_path`].
pub fn head_path() -> &'static str {
    first_existing(&["/bin/head", "/usr/bin/head"])
}

/// Absolute path to an external `kill` for contract cleanup. See [`true_path`].
pub fn kill_path() -> &'static str {
    first_existing(&["/bin/kill", "/usr/bin/kill"])
}

/// Absolute path to an external `sh` for helper scripts. See [`true_path`].
pub fn sh_path() -> &'static str {
    process::sh_path()
}

fn first_existing(candidates: &'static [&'static str]) -> &'static str {
    candidates
        .iter()
        .copied()
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| panic!("none of {candidates:?} exist on this system"))
}

pub fn run_command(command: &str) -> Output {
    run_dsh(["-c", command], Duration::from_secs(10))
}

pub fn run_interactive(lines: &[&str]) -> Output {
    let mut input = lines.join("\n");
    input.push_str("\nexit\n");
    run_dsh_with_input(
        std::iter::empty::<&str>(),
        Some(&input),
        Duration::from_secs(10),
    )
}
