//! Completion helper subprocesses: spawn a helper with piped stdout, drain it
//! under an overall runtime deadline, and classify the outcome as completed,
//! non-zero exit, runtime timeout, or stdout-limit exhaustion. Timeouts and
//! limit exhaustion kill the whole process group and reap the child; only a
//! genuinely completed process yields a successful (possibly empty) result.
//! The limit is inclusive: exactly MAX_STDOUT_BYTES is valid; the first byte
//! beyond it is overflow.

use anyhow::Result;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READ_POLL_INTERVAL: Duration = Duration::from_millis(5);
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(200);
pub(crate) const MAX_STDOUT_BYTES: usize = 1024 * 1024;

/// Typed outcome of a completion helper subprocess.
///
/// `Completed` is the only successful outcome; every other variant describes a
/// normal process event that the caller must classify. Spawn/read/wait system
/// failures and invalid UTF-8 remain `Result::Err`.
#[derive(Debug)]
pub(crate) enum CollectStdoutOutcome {
    Completed(String),
    NonZeroExit {
        // Kept typed (not yet surfaced to diagnostics: non-zero exit stays
        // soft-empty for compatibility). A future task can report exit codes
        // without re-plumbing the subprocess layer.
        #[allow(dead_code)]
        status: std::process::ExitStatus,
    },
    TimedOut {
        timeout: Duration,
    },
    OutputLimitExceeded {
        limit: usize,
    },
}

#[cfg(test)]
static EXTERNAL_PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn external_process_test_guard() -> std::sync::MutexGuard<'static, ()> {
    EXTERNAL_PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainStatus {
    Eof,
    WouldBlock,
    DeadlineReached,
    OutputLimitExceeded,
}

pub(crate) fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    command
}

/// Wrap a value in single quotes so it is treated as a single literal argument
/// by a POSIX shell. Any embedded single quote is escaped using the standard
/// `'\''` sequence. Use this before splicing any user-controlled value into a
/// string that is later executed via `sh -c` (script completion templates,
/// preview commands, etc.).
pub(crate) fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub(crate) fn shell_command(command_template: &str) -> Command {
    let mut cmd = command("sh");
    cmd.arg("-c").arg(command_template);
    cmd
}

pub(crate) fn collect_stdout_outcome(
    mut command: Command,
    timeout: Duration,
) -> Result<CollectStdoutOutcome> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    wait_and_collect_stdout(&mut child, timeout)
}

/// Explicitly lossy compatibility wrapper: timeout, output-limit exhaustion
/// and non-zero exit all collapse to an empty string, preserving the historic
/// script-completion behavior. Dynamic completion must use
/// `collect_stdout_outcome` instead so timeouts are not cached as successes.
pub(crate) fn collect_stdout_or_empty(command: Command, timeout: Duration) -> Result<String> {
    match collect_stdout_outcome(command, timeout)? {
        CollectStdoutOutcome::Completed(stdout) => Ok(stdout),
        CollectStdoutOutcome::NonZeroExit { .. }
        | CollectStdoutOutcome::TimedOut { .. }
        | CollectStdoutOutcome::OutputLimitExceeded { .. } => Ok(String::new()),
    }
}

fn wait_and_collect_stdout(child: &mut Child, timeout: Duration) -> Result<CollectStdoutOutcome> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Child stdout not captured"))?;
    set_nonblocking(&stdout)?;

    let started = Instant::now();
    let deadline = deadline_after(started, timeout);
    let mut output = Vec::new();

    loop {
        match drain_available_stdout(&mut stdout, &mut output, deadline)? {
            DrainStatus::DeadlineReached => {
                terminate_child(child);
                let _ = child.wait();
                return Ok(CollectStdoutOutcome::TimedOut { timeout });
            }
            DrainStatus::OutputLimitExceeded => {
                terminate_child(child);
                let _ = child.wait();
                return Ok(CollectStdoutOutcome::OutputLimitExceeded {
                    limit: MAX_STDOUT_BYTES,
                });
            }
            DrainStatus::Eof | DrainStatus::WouldBlock => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let drain_status = drain_stdout_after_exit(&mut stdout, &mut output)?;
                if drain_status == DrainStatus::OutputLimitExceeded {
                    terminate_child(child);
                    return Ok(CollectStdoutOutcome::OutputLimitExceeded {
                        limit: MAX_STDOUT_BYTES,
                    });
                }
                if drain_status == DrainStatus::DeadlineReached {
                    // The child has already exited here; EXIT_DRAIN_GRACE expiry is an
                    // output-ownership boundary, not a command runtime timeout.
                    // Typically a background descendant still holds the stdout
                    // write end open. Clean up descendants, then report whatever
                    // the canonical child produced as a normal completion.
                    terminate_child(child);
                }
                return if status.success() {
                    Ok(CollectStdoutOutcome::Completed(String::from_utf8(output)?))
                } else {
                    Ok(CollectStdoutOutcome::NonZeroExit { status })
                };
            }
            Ok(None) => {}
            Err(err) if child_was_reaped(&err) => {
                let drain_status = drain_stdout_after_exit(&mut stdout, &mut output)?;
                if drain_status == DrainStatus::OutputLimitExceeded {
                    terminate_child(child);
                    return Ok(CollectStdoutOutcome::OutputLimitExceeded {
                        limit: MAX_STDOUT_BYTES,
                    });
                }
                if drain_status == DrainStatus::DeadlineReached {
                    // Same output-ownership boundary as above: canonical status is
                    // unavailable (ECHILD), so report drained output as completed.
                    terminate_child(child);
                }
                return Ok(CollectStdoutOutcome::Completed(String::from_utf8(output)?));
            }
            Err(err) => return Err(err.into()),
        }

        if started.elapsed() >= timeout {
            terminate_child(child);
            let _ = child.wait();
            return Ok(CollectStdoutOutcome::TimedOut { timeout });
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        std::thread::sleep(remaining.min(READ_POLL_INTERVAL));
    }
}

fn deadline_after(started: Instant, timeout: Duration) -> Instant {
    started.checked_add(timeout).unwrap_or(started)
}

#[cfg(unix)]
fn child_was_reaped(err: &io::Error) -> bool {
    err.raw_os_error() == Some(libc::ECHILD)
}

#[cfg(not(unix))]
fn child_was_reaped(_err: &io::Error) -> bool {
    false
}

fn drain_stdout_after_exit(
    stdout: &mut std::process::ChildStdout,
    output: &mut Vec<u8>,
) -> io::Result<DrainStatus> {
    let deadline = Instant::now() + EXIT_DRAIN_GRACE;
    loop {
        let before = output.len();
        match drain_available_stdout(stdout, output, deadline)? {
            DrainStatus::Eof => return Ok(DrainStatus::Eof),
            DrainStatus::DeadlineReached => return Ok(DrainStatus::DeadlineReached),
            DrainStatus::OutputLimitExceeded => return Ok(DrainStatus::OutputLimitExceeded),
            DrainStatus::WouldBlock => {}
        }
        if Instant::now() >= deadline {
            return Ok(DrainStatus::DeadlineReached);
        }
        if output.len() == before {
            std::thread::sleep(READ_POLL_INTERVAL);
        }
    }
}

fn drain_available_stdout(
    stdout: &mut std::process::ChildStdout,
    output: &mut Vec<u8>,
    deadline: Instant,
) -> io::Result<DrainStatus> {
    let mut buf = [0_u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Ok(DrainStatus::DeadlineReached);
        }

        debug_assert!(output.len() <= MAX_STDOUT_BYTES);

        let remaining = MAX_STDOUT_BYTES - output.len();
        let read_len = buf.len().min(remaining.saturating_add(1));

        debug_assert!(read_len > 0);

        match stdout.read(&mut buf[..read_len]) {
            Ok(0) => return Ok(DrainStatus::Eof),
            Ok(n) if n > remaining => {
                return Ok(DrainStatus::OutputLimitExceeded);
            }
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DrainStatus::WouldBlock);
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(stdout: &std::process::ChildStdout) -> io::Result<()> {
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_nonblocking(_stdout: &std::process::ChildStdout) -> io::Result<()> {
    Ok(())
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn shell_quote_wraps_plain_value() {
        assert_eq!(shell_quote("br"), "'br'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quote() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[cfg(unix)]
    fn write_executable_script(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    fn script_command(path: &Path) -> std::process::Command {
        let mut command = command("sh");
        command.arg(path);
        command
    }

    #[test]
    fn successful_stdout_is_completed() {
        let outcome =
            collect_stdout_outcome(shell_command("printf ok"), Duration::from_millis(1500))
                .unwrap();

        assert!(
            matches!(outcome, CollectStdoutOutcome::Completed(ref stdout) if stdout == "ok"),
            "expected Completed(\"ok\"), got {outcome:?}"
        );
    }

    #[test]
    fn successful_empty_stdout_is_completed_empty() {
        // The opposite case of a timeout: exit 0 with no output is a
        // legitimate empty result, not a failure.
        let outcome =
            collect_stdout_outcome(shell_command("exit 0"), Duration::from_millis(1500)).unwrap();

        assert!(
            matches!(outcome, CollectStdoutOutcome::Completed(ref stdout) if stdout.is_empty()),
            "expected Completed(\"\"), got {outcome:?}"
        );
    }

    #[test]
    fn non_zero_exit_is_typed_but_distinct_from_timeout() {
        let outcome =
            collect_stdout_outcome(shell_command("exit 7"), Duration::from_millis(1500)).unwrap();

        match outcome {
            CollectStdoutOutcome::NonZeroExit { status } => {
                assert_eq!(status.code(), Some(7));
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[test]
    fn slow_producer_hits_runtime_timeout_not_empty_success() {
        // A slow drip stays far below the output limit, so only the runtime
        // deadline can fire here.
        let started = Instant::now();
        let outcome = collect_stdout_outcome(
            shell_command("while :; do printf x; sleep 0.05; done"),
            Duration::from_millis(200),
        )
        .unwrap();

        assert!(
            matches!(outcome, CollectStdoutOutcome::TimedOut { .. }),
            "expected TimedOut, got {outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn write_repeated_file(path: &Path, len: usize, byte: u8) {
        let chunk = vec![byte; 8192];
        let mut file = fs::File::create(path).unwrap();
        let mut remaining = len;

        while remaining > 0 {
            let take = chunk.len().min(remaining);
            std::io::Write::write_all(&mut file, &chunk[..take]).unwrap();

            remaining -= take;
        }
    }

    #[test]
    fn stdout_exactly_at_limit_is_completed() {
        // Inclusive ceiling: exactly MAX bytes followed by EOF is success.
        // The file content is fixed before the child spawns, so no sleep
        // synchronization is needed.
        let dir = tempdir().unwrap();
        let exact = dir.path().join("exact.txt");
        write_repeated_file(&exact, MAX_STDOUT_BYTES, b'x');

        let mut command = command("cat");
        command.arg(&exact);
        let outcome = collect_stdout_outcome(command, Duration::from_secs(30)).unwrap();

        match outcome {
            CollectStdoutOutcome::Completed(stdout) => {
                assert_eq!(stdout.len(), MAX_STDOUT_BYTES);
                assert!(stdout.bytes().all(|byte| byte == b'x'));
            }
            CollectStdoutOutcome::OutputLimitExceeded { .. } => {
                panic!("exact limit must not be classified as overflow");
            }
            CollectStdoutOutcome::TimedOut { .. } => {
                panic!("exact limit unexpectedly timed out");
            }
            CollectStdoutOutcome::NonZeroExit { status } => {
                panic!("cat unexpectedly exited with {status}");
            }
        }
    }

    #[test]
    fn stdout_one_byte_over_limit_is_rejected() {
        // Opposite boundary: MAX + 1 bytes must observe the overflow byte and
        // fail as OutputLimitExceeded, not time out.
        let dir = tempdir().unwrap();
        let over = dir.path().join("over.txt");
        write_repeated_file(&over, MAX_STDOUT_BYTES + 1, b'x');

        let mut command = command("cat");
        command.arg(&over);
        let outcome = collect_stdout_outcome(command, Duration::from_secs(30)).unwrap();

        match outcome {
            CollectStdoutOutcome::OutputLimitExceeded { limit } => {
                assert_eq!(limit, MAX_STDOUT_BYTES);
            }
            CollectStdoutOutcome::Completed(_) => {
                panic!("max-plus-one output must not complete successfully");
            }
            CollectStdoutOutcome::TimedOut { .. } => {
                panic!("max-plus-one output unexpectedly timed out");
            }
            CollectStdoutOutcome::NonZeroExit { status } => {
                panic!("cat unexpectedly exited with {status}");
            }
        }
    }

    #[test]
    fn lossy_wrapper_preserves_empty_compatibility() {
        // Script completion keeps the historic behavior: timeout, output
        // limit and non-zero exit all collapse to an empty string.
        assert_eq!(
            collect_stdout_or_empty(
                shell_command("while :; do printf x; sleep 0.05; done"),
                Duration::from_millis(100),
            )
            .unwrap(),
            ""
        );
        assert_eq!(
            collect_stdout_or_empty(shell_command("exit 7"), Duration::from_millis(1500)).unwrap(),
            ""
        );
        assert_eq!(
            collect_stdout_or_empty(shell_command("exit 0"), Duration::from_millis(1500)).unwrap(),
            ""
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_descendants_in_process_group() {
        let _guard = external_process_test_guard();
        let dir = tempdir().unwrap();
        let script = dir.path().join("holds-stdout.sh");
        let survived = dir.path().join("survived.txt");
        write_executable_script(
            &script,
            "#!/bin/sh\n(sleep 2; printf survived > survived.txt) &\nwait\n",
        );

        let mut command = script_command(&script);
        command.current_dir(dir.path());
        let started = Instant::now();
        let outcome = collect_stdout_outcome(command, Duration::from_millis(300)).unwrap();

        assert!(
            matches!(outcome, CollectStdoutOutcome::TimedOut { .. }),
            "expected TimedOut, got {outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1800));
        assert!(
            !survived.exists(),
            "timeout should kill descendant processes in the subprocess group"
        );
    }

    #[cfg(unix)]
    #[test]
    fn successful_child_does_not_wait_for_stdout_holding_background_process() {
        // The canonical child already exited here, so the post-exit drain
        // grace expiring is an output-ownership boundary, not a runtime
        // timeout: this must stay Completed, never TimedOut.
        let dir = tempdir().unwrap();
        let script = dir.path().join("background-stdout.sh");
        write_executable_script(&script, "#!/bin/sh\n(sleep 4; printf late) &\nexit 0\n");

        let mut command = script_command(&script);
        command.current_dir(dir.path());
        let started = Instant::now();
        let outcome = collect_stdout_outcome(command, Duration::from_millis(1500)).unwrap();

        assert!(
            matches!(outcome, CollectStdoutOutcome::Completed(ref stdout) if stdout.is_empty()),
            "expected Completed(\"\"), got {outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
