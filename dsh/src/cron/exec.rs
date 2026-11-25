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
//! 3. **Execution is synchronous.** A run happens inside `cron run-job`,
//!    which waits for the detached shell command while keeping the session
//!    runtime out of the child process.

use anyhow::{Context as _, Result};
use dsh_types::cron::job::ClaimedRun;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
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

    // Internal interpreter boundary: cron jobs run through the fixed
    // system shell, never a PATH-resolved `sh`.
    let mut command = Command::new("/bin/sh");
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

/// Keep the group leader waitable until both output pipes close. Its PID
/// pins the group identity if a descendant outlives the shell and keeps a
/// writer open past the deadline.
fn wait_with_deadline(mut child: Child, timeout: Duration, started: Instant) -> CommandOutcome {
    let mut pipes = [
        child.stdout.take().map(OwnedFd::from).map(File::from),
        child.stderr.take().map(OwnedFd::from).map(File::from),
    ];
    let mut output = [Vec::new(), Vec::new()];
    let mut timed_out = false;
    let status = loop {
        if pipes.iter().all(Option::is_none) {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(_) => break None,
            }
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            // Account for bytes already in the pipes before deciding whether
            // the child finished in time. A live writer must not extend this
            // final drain by producing more data.
            drain_buffered(&mut pipes, &mut output);
            if pipes.iter().all(Option::is_none)
                && let Ok(Some(status)) = child.try_wait()
            {
                break Some(status);
            }
            timed_out = true;
        } else if drain_ready(&mut pipes, &mut output, remaining.min(POLL_INTERVAL)).is_ok() {
            continue;
        }
        kill_group(&child);
        let _ = child.wait();
        break None;
    };

    let [stdout, stderr] = output.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    CommandOutcome {
        stdout,
        stderr,
        exit_code: if timed_out {
            TIMED_OUT
        } else {
            status
                .and_then(|status| status.code())
                .unwrap_or(SPAWN_FAILED)
        },
        timed_out,
        duration: started.elapsed(),
    }
}

fn drain_buffered(pipes: &mut [Option<File>; 2], output: &mut [Vec<u8>; 2]) {
    for (pipe, bytes) in pipes.iter_mut().zip(output.iter_mut()) {
        if let Some(reader) = pipe {
            let mut available: libc::c_int = 0;
            // FIONREAD snapshots the number of buffered bytes; this is
            // the sole reader, so reading up to that many cannot block.
            if unsafe { libc::ioctl(reader.as_raw_fd(), libc::FIONREAD, &mut available) } == 0 {
                let _ = reader.take(available.max(0) as u64).read_to_end(bytes);
            }
        }
    }
    // Probe EOF once, without waiting for future writes.
    let _ = drain_ready(pipes, output, Duration::ZERO);
}

fn drain_ready(
    pipes: &mut [Option<File>; 2],
    output: &mut [Vec<u8>; 2],
    wait: Duration,
) -> io::Result<()> {
    let mut descriptors = pipes.each_ref().map(|pipe| libc::pollfd {
        fd: pipe.as_ref().map_or(-1, AsRawFd::as_raw_fd),
        events: libc::POLLIN,
        revents: 0,
    });
    // The descriptors borrow the two live pipe readers. A negative fd
    // tells poll to ignore a stream which already reached EOF.
    let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, wait.as_millis() as i32) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::Interrupted {
            Ok(())
        } else {
            Err(error)
        };
    }
    let mut buffer = [0; 16384];
    for (index, descriptor) in descriptors.iter().enumerate() {
        if descriptor.revents != 0
            && let Some(pipe) = &mut pipes[index]
        {
            match pipe.read(&mut buffer) {
                Ok(0) => pipes[index] = None,
                Ok(n) => output[index].extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
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
/// run needs - the command, the environment - is read back from the store by
/// the child. The actual spawn is shared - see `crate::detached_child`.
pub fn spawn_run_child(run_id: &str) -> Result<Child> {
    uuid::Uuid::parse_str(run_id).context("run id is not a UUID")?;
    crate::detached_child::spawn(
        &format!("cron run-job {run_id}"),
        Stdio::null(),
        Stdio::null(),
    )
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
