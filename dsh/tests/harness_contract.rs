//! Regression tests for the total-deadline output collector.
//!
//! The contract harness bounds primary process exit **and** capture-pipe
//! EOF as one absolute deadline. These tests pin that contract:
//!
//! - a descendant-held pipe after primary exit is an `OutputEof` timeout,
//!   not indefinite success;
//! - legitimate late output before the deadline is still collected (the
//!   harness never kills background descendants on primary exit);
//! - partial output survives a timeout;
//! - a stderr-only held pipe reports split EOF state;
//! - large output never deadlocks the capture loop.

mod common;

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

use common::process::{
    WaitError, WaitTimeoutPhase, spawn_dsh_unlocked, wait_child_with_output_deadline,
};

/// Extract the numeric PID following `marker` in `haystack`.
fn parse_pid_after_marker(haystack: &[u8], marker: &str) -> Option<Pid> {
    let text = String::from_utf8_lossy(haystack);
    let start = text.find(marker)? + marker.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<i32>().ok().map(Pid::from_raw)
}

/// Best-effort termination of a leaked helper. Never panics; the caller
/// asserts afterwards so a cleanup failure cannot mask the real verdict.
fn cleanup_helper(pid: Pid) {
    let _ = killpg(pid, Signal::SIGKILL);
    let _ = kill(pid, Signal::SIGKILL);
    std::thread::sleep(Duration::from_millis(200));
}

/// Primary exits at once, but the async helper holds the capture pipes.
/// The case deadline must fire as `OutputEof`, not succeed when the helper
/// finally exits.
#[test]
fn held_pipe_after_primary_exit_is_output_eof_timeout() {
    let timeout = Duration::from_millis(500);
    let process = spawn_dsh_unlocked(
        ["-c".to_string(), "sleep 10 & echo PID:$!".to_string()],
        None,
    );
    let started = Instant::now();
    match process.wait(timeout) {
        Ok(output) => panic!(
            "expected OutputEof timeout, got success: status={:?} stdout={:?} stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(WaitError::TimedOut {
            output,
            phase,
            stdout_eof,
            stderr_eof,
        }) => {
            let elapsed = started.elapsed();
            if let Some(pid) = parse_pid_after_marker(&output.stdout, "PID:") {
                cleanup_helper(pid);
            }
            assert_eq!(
                phase,
                WaitTimeoutPhase::OutputEof,
                "phase must be output-eof"
            );
            assert!(!stdout_eof, "stdout must still be held open");
            assert!(!stderr_eof, "stderr must still be held open");
            // Loose upper bound only: the old `wait_with_output` path would
            // block ~10s until the helper exits.
            assert!(
                elapsed < Duration::from_secs(5),
                "collector must stop at the deadline, took {elapsed:?}"
            );
        }
        Err(WaitError::Io(err)) => panic!("collector I/O failure: {err}"),
    }
}

/// The reverse direction: a helper that writes shortly after primary exit
/// must have its output collected. This forbids kill-on-primary-exit.
#[test]
fn legitimate_late_output_after_primary_exit_is_collected() {
    let process = spawn_dsh_unlocked(
        [
            "-c".to_string(),
            "sleep 1 && echo LATE & echo EARLY".to_string(),
        ],
        None,
    );
    match process.wait(Duration::from_secs(8)) {
        Ok(output) => {
            assert!(output.status.success(), "status must be success");
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            assert!(stdout.contains("EARLY"), "missing EARLY in {stdout:?}");
            assert!(stdout.contains("LATE"), "missing late LATE in {stdout:?}");
        }
        Err(WaitError::TimedOut {
            output,
            phase,
            stdout_eof,
            stderr_eof,
        }) => panic!(
            "late output must be collected before the deadline (phase: {}, stdout eof: {stdout_eof}, stderr eof: {stderr_eof})\npartial stdout:\n{}\npartial stderr:\n{}",
            phase.as_str(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(WaitError::Io(err)) => panic!("collector I/O failure: {err}"),
    }
}

/// A timeout must keep what arrived before the deadline for diagnostics.
#[test]
fn timeout_preserves_partial_output() {
    let timeout = Duration::from_millis(500);
    let process = spawn_dsh_unlocked(
        [
            "-c".to_string(),
            "echo BEFORE; sleep 10 & echo PID:$!".to_string(),
        ],
        None,
    );
    match process.wait(timeout) {
        Ok(output) => panic!(
            "expected timeout, got success: {:?}",
            String::from_utf8_lossy(&output.stdout)
        ),
        Err(WaitError::TimedOut {
            output,
            phase,
            stdout_eof,
            stderr_eof,
        }) => {
            if let Some(pid) = parse_pid_after_marker(&output.stdout, "PID:") {
                cleanup_helper(pid);
            }
            assert_eq!(
                phase,
                WaitTimeoutPhase::OutputEof,
                "phase must be output-eof"
            );
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            assert!(
                stdout.contains("BEFORE"),
                "partial stdout must keep BEFORE, got {stdout:?}"
            );
            assert!(!stdout_eof && !stderr_eof, "both pipes must be held");
        }
        Err(WaitError::Io(err)) => panic!("collector I/O failure: {err}"),
    }
}

/// Split EOF: stdout closed, stderr held open by a surviving grandchild.
///
/// dogesh async helpers always hold both capture pipes for the job's
/// lifetime, so this shape is built with a raw `/bin/sh` child instead:
/// it prints `READY` to stdout, closes stdout, spawns a `sleep` that
/// inherits only stderr, then exits. The primary is gone while stderr is
/// still held — the macOS CI #131 shape.
#[test]
fn stderr_only_held_pipe_reports_split_eof() {
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg("echo READY; exec 1>&-; sleep 10 & echo $! >&2")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn raw sh helper");
    let pgid = Pid::from_raw(child.id() as i32);
    let timeout = Duration::from_millis(500);
    let started = Instant::now();
    match wait_child_with_output_deadline(child, pgid, timeout) {
        Ok(output) => panic!(
            "expected OutputEof timeout, got success: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(WaitError::TimedOut {
            output,
            phase,
            stdout_eof,
            stderr_eof,
        }) => {
            let elapsed = started.elapsed();
            // stderr holds the grandchild PID as its whole content; parse
            // leniently and clean it up best-effort.
            let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();
            if let Some(pid) = stderr_text
                .split_whitespace()
                .filter_map(|token| token.parse::<i32>().ok())
                .map(Pid::from_raw)
                .next()
            {
                cleanup_helper(pid);
            }
            assert_eq!(
                phase,
                WaitTimeoutPhase::OutputEof,
                "phase must be output-eof"
            );
            assert!(stdout_eof, "stdout must have reached EOF");
            assert!(!stderr_eof, "stderr must still be held open");
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            assert!(
                stdout.contains("READY"),
                "partial stdout must keep READY, got {stdout:?}"
            );
            assert!(
                elapsed < Duration::from_secs(5),
                "collector must stop at the deadline, took {elapsed:?}"
            );
        }
        Err(WaitError::Io(err)) => panic!("collector I/O failure: {err}"),
    }
}

/// Output far beyond the pipe buffer on both streams must flow without the
/// capture loop deadlocking the child.
#[test]
fn large_output_does_not_deadlock() {
    let yes = common::yes_path().to_string();
    let head = common::head_path().to_string();
    let script = format!("{yes} | {head} -c 300000; {yes} | {head} -c 300000 1>&2; echo DONE");
    let process = spawn_dsh_unlocked(["-c".to_string(), script], None);
    match process.wait(Duration::from_secs(15)) {
        Ok(output) => {
            assert!(output.status.success(), "status must be success");
            assert!(
                output.stdout.len() > 200_000,
                "stdout must exceed the pipe buffer, got {} bytes",
                output.stdout.len()
            );
            assert!(
                output.stderr.len() > 200_000,
                "stderr must exceed the pipe buffer, got {} bytes",
                output.stderr.len()
            );
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            assert!(stdout.contains("DONE"), "missing DONE in stdout tail");
        }
        Err(WaitError::TimedOut {
            output,
            phase,
            stdout_eof,
            stderr_eof,
        }) => panic!(
            "large output must not deadlock (phase: {}, stdout eof: {stdout_eof}, stderr eof: {stderr_eof}, stdout {} bytes, stderr {} bytes)",
            phase.as_str(),
            output.stdout.len(),
            output.stderr.len()
        ),
        Err(WaitError::Io(err)) => panic!("collector I/O failure: {err}"),
    }
}
