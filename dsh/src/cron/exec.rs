//! Running one job's command, and starting the child that runs a claimed run.
//!
//! Adapted from the `sched` runner, with three deliberate differences:
//!
//! 1. **The digest is stable across toolchains.** `sched` hashed with
//!    `DefaultHasher`, which is fine for a value that lives in memory for one
//!    session. Cron persists it, and `DefaultHasher` is explicitly not stable
//!    between Rust releases - so upgrading the compiler would change every
//!    stored digest and make every `--on change` job announce itself once, for
//!    nothing. FNV-1a, the same choice `skills::trust` made for the same
//!    reason.
//! 2. **Children outlive the shell.** `sched` set `kill_on_drop`, because a
//!    scheduled task was session-scoped by definition. A cron run is not: it
//!    may have been started by a session that has since been closed, and
//!    killing it at exit would make "runs while you are logged out" untrue.
//! 3. **Execution is synchronous.** A run happens inside `cron run-job`, a
//!    builtin that also has to be able to call the agent entry point, which
//!    needs `&mut Shell`. Reader threads plus a polled deadline keep that
//!    possible without a second runtime.

use anyhow::{Context as _, Result};
use dsh_types::cron::job::ClaimedRun;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Exit code reported when the command could not be started at all.
pub const SPAWN_FAILED: i32 = 127;
/// Exit code reported for a timeout, matching the shell convention for
/// SIGKILL (128 + 9).
pub const TIMED_OUT: i32 = 137;

/// Characters of the first output line kept for a run's preview.
const PREVIEW_CHARS: usize = 120;

/// How often the deadline is checked while a command runs.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct CommandOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration: Duration,
}

/// Executes a shell job and waits for it, up to its timeout.
///
/// The child is isolated from any interactive session the way `sched`'s was:
/// `stdin` on `/dev/null` so it can never read from - or steal - the terminal,
/// and its own process group so a `Ctrl-C` at somebody's prompt does not reach
/// it. The environment is the snapshot taken when the job was created, not
/// whatever the process that happened to tick it was carrying.
pub fn run_command(run: &ClaimedRun) -> CommandOutcome {
    let started = Instant::now();
    let timeout = Duration::from_secs(run.timeout_secs.max(1));

    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&run.command)
        .current_dir(&run.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command.env_clear();
    command.envs(&run.env);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandOutcome {
                stdout: String::new(),
                stderr: format!("cron: failed to start: {error}\n"),
                exit_code: SPAWN_FAILED,
                timed_out: false,
                duration: started.elapsed(),
            };
        }
    };

    wait_with_deadline(child, timeout, started)
}

/// Waits for `child`, draining both pipes from their own threads.
///
/// Reading after the wait would deadlock the moment a command produces more
/// than a pipe buffer, which is exactly the sort of job someone schedules.
fn wait_with_deadline(mut child: Child, timeout: Duration, started: Instant) -> CommandOutcome {
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_to_string(&mut stdout_pipe));
    let stderr_reader = std::thread::spawn(move || read_to_string(&mut stderr_pipe));

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    timed_out = true;
                    // The child has its own process group, so the whole
                    // pipeline goes, not just `sh`.
                    kill_group(&child);
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let exit_code = if timed_out {
        TIMED_OUT
    } else {
        // A signalled child has no exit code; report it the way the shell does
        // elsewhere rather than inventing a success.
        status
            .and_then(|status| status.code())
            .unwrap_or(SPAWN_FAILED)
    };

    CommandOutcome {
        stdout,
        stderr,
        exit_code,
        timed_out,
        duration: started.elapsed(),
    }
}

fn read_to_string<R: Read>(pipe: &mut Option<R>) -> String {
    let Some(pipe) = pipe.as_mut() else {
        return String::new();
    };
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

fn kill_group(child: &Child) {
    let pid = child.id() as i32;
    // `process_group(0)` made the child its own leader, so its pgid is its pid.
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
}

/// Starts the child that will execute one claimed run.
///
/// The only thing crossing the boundary is a UUID, validated here so that a
/// corrupt row cannot put anything else on a command line. Everything else the
/// run needs - the goal, the grant, the environment - is read back from the
/// store by the child.
pub fn spawn_run_child(run_id: &str) -> Result<Child> {
    uuid::Uuid::parse_str(run_id).context("run id is not a UUID")?;
    let program = std::env::current_exe().context("cannot find this dogesh binary")?;

    Command::new(program)
        .arg("-c")
        .arg(format!("cron run-job {run_id}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("cannot start the cron run child")
}

/// Hashes output for change detection, stably across toolchains.
///
/// ANSI escapes and trailing whitespace are stripped first: a command that
/// colours its output or pads a column would otherwise look different on every
/// run for reasons nobody asked about.
pub fn digest(output: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for line in output.lines() {
        for byte in console::strip_ansi_codes(line).trim_end().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= u64::from(b'\n');
        hash = hash.wrapping_mul(PRIME);
    }
    hash
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
mod tests;
