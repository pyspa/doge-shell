//! Runs one scheduled command, fully detached from the interactive shell.

use super::DueTask;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

/// Exit code reported when the command could not be started at all.
pub const SPAWN_FAILED: i32 = 127;
/// Exit code reported for a timeout, matching the shell convention for
/// SIGKILL (128 + 9).
pub const TIMED_OUT: i32 = 137;

/// Characters of the first output line kept for `sched list` / `sched log`.
const PREVIEW_CHARS: usize = 60;

pub struct RunOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration: Duration,
}

/// Executes `task` and waits for it, up to its timeout.
///
/// The child is deliberately isolated from the interactive session:
///
/// - `stdin` is `/dev/null`, so it can never read from — or steal — the
///   terminal while the user is typing.
/// - it runs in its own process group, so `Ctrl-C` at the prompt does not
///   reach it.
/// - `kill_on_drop` cleans up if the shell exits mid-run.
pub async fn run(task: &DueTask) -> RunOutcome {
    let started = Instant::now();

    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);

    command.env_clear();
    command.envs(&task.env);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return RunOutcome {
                stdout: String::new(),
                stderr: format!("sched: failed to start: {err}\n"),
                exit_code: SPAWN_FAILED,
                timed_out: false,
                duration: started.elapsed(),
            };
        }
    };

    match tokio::time::timeout(task.timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => RunOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            // A signalled child has no exit code; report it the way the shell
            // does elsewhere (128 + signal is not recoverable here, so fall
            // back to the generic failure code).
            exit_code: output.status.code().unwrap_or(SPAWN_FAILED),
            timed_out: false,
            duration: started.elapsed(),
        },
        Ok(Err(err)) => RunOutcome {
            stdout: String::new(),
            stderr: format!("sched: {err}\n"),
            exit_code: SPAWN_FAILED,
            timed_out: false,
            duration: started.elapsed(),
        },
        Err(_) => {
            // `wait_with_output` consumed the child, so the timeout drop is what
            // kills it — `kill_on_drop` above makes that reliable.
            debug!("sched task '{}' timed out", task.name);
            RunOutcome {
                stdout: String::new(),
                stderr: format!("sched: timed out after {:?}\n", task.timeout),
                exit_code: TIMED_OUT,
                timed_out: true,
                duration: started.elapsed(),
            }
        }
    }
}

/// Hashes output for change detection.
///
/// ANSI escapes and trailing whitespace are stripped first: a command that
/// colours its output or pads a column would otherwise look different on every
/// run for reasons the user does not care about.
pub fn digest(output: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    for line in output.lines() {
        console::strip_ansi_codes(line).trim_end().hash(&mut hasher);
    }
    hasher.finish()
}

/// First non-empty output line, truncated for single-line display.
pub fn preview(output: &str) -> String {
    let Some(line) = output
        .lines()
        .map(|line| console::strip_ansi_codes(line).trim().to_string())
        .find(|line| !line.is_empty())
    else {
        return String::new();
    };

    if line.chars().count() <= PREVIEW_CHARS {
        return line;
    }
    let truncated: String = line.chars().take(PREVIEW_CHARS - 1).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn task(command: &str, timeout: Duration) -> DueTask {
        DueTask {
            id: 1,
            name: "test".to_string(),
            command: command.to_string(),
            cwd: "/tmp".to_string(),
            timeout,
            env: HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let outcome = run(&task("echo hello", Duration::from_secs(10))).await;
        assert_eq!(outcome.stdout.trim(), "hello");
        assert_eq!(outcome.exit_code, 0);
        assert!(!outcome.timed_out);
    }

    #[tokio::test]
    async fn reports_a_failing_command() {
        let outcome = run(&task("exit 3", Duration::from_secs(10))).await;
        assert_eq!(outcome.exit_code, 3);
    }

    #[tokio::test]
    async fn captures_stderr_separately() {
        let outcome = run(&task("echo oops >&2", Duration::from_secs(10))).await;
        assert!(outcome.stdout.is_empty());
        assert_eq!(outcome.stderr.trim(), "oops");
    }

    #[tokio::test]
    async fn runs_in_the_configured_directory() {
        let mut due = task("pwd", Duration::from_secs(10));
        due.cwd = "/".to_string();
        let outcome = run(&due).await;
        assert_eq!(outcome.stdout.trim(), "/");
    }

    #[tokio::test]
    async fn a_hung_command_times_out() {
        let outcome = run(&task("sleep 30", Duration::from_millis(200))).await;
        assert!(outcome.timed_out);
        assert_eq!(outcome.exit_code, TIMED_OUT);
        assert!(outcome.duration < Duration::from_secs(5));
    }

    /// stdin must be closed, not inherited: a task that reads would otherwise
    /// swallow the user's keystrokes.
    #[tokio::test]
    async fn stdin_is_empty() {
        let outcome = run(&task("cat; echo done", Duration::from_secs(5))).await;
        assert!(!outcome.timed_out);
        assert_eq!(outcome.stdout.trim(), "done");
    }

    #[tokio::test]
    async fn a_missing_directory_is_reported_not_panicked() {
        let mut due = task("true", Duration::from_secs(5));
        due.cwd = "/nonexistent-dir-for-dsh-test".to_string();
        let outcome = run(&due).await;
        assert_eq!(outcome.exit_code, SPAWN_FAILED);
        assert!(outcome.stderr.contains("failed to start"));
    }

    #[test]
    fn digest_ignores_colour_and_trailing_space() {
        assert_eq!(digest("hello"), digest("hello   "));
        assert_eq!(digest("hello"), digest("\u{1b}[31mhello\u{1b}[0m"));
        assert_ne!(digest("hello"), digest("goodbye"));
        assert_ne!(digest("a\nb"), digest("a"));
    }

    #[test]
    fn preview_takes_the_first_meaningful_line() {
        assert_eq!(preview("\n\n  hello \nworld"), "hello");
        assert_eq!(preview(""), "");
        assert_eq!(preview("   \n  "), "");
        assert_eq!(preview("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn preview_truncates_long_lines_on_a_char_boundary() {
        let long = "あ".repeat(200);
        let preview = preview(&long);
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
        assert!(preview.ends_with('…'));
    }
}
