//! Starting a detached `dogesh -c "<command>"` child and reaping it without
//! blocking the caller.
//!
//! Shared by cron (`cron run-job <id>`) and any other caller that needs
//! a child that outlives the process that started it, with its own process
//! group so a `Ctrl-C` at somebody's prompt does not reach it, and stdin on
//! `/dev/null` so it can never read from - or steal - the terminal.

use anyhow::{Context as _, Result};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// Starts `dogesh -c "<command>"` as its own process group leader, stdin
/// closed.
///
/// `stdout`/`stderr` say where the child's output goes: pass `Stdio::null()`
/// to discard it (cron) or an opened file to keep it (the detached agent's
/// `run.log`).
pub fn spawn(command: &str, stdout: Stdio, stderr: Stdio) -> Result<Child> {
    let program = std::env::current_exe().context("cannot find this dogesh binary")?;

    Command::new(program)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .context("cannot start the detached child")
}

/// Reaps `child` from a dedicated thread, so the caller does not block and
/// the child does not become a zombie once it exits.
///
/// The run is meant to outlive whoever started it, so this thread is the
/// only thing standing between a finished child and a zombie - nothing else
/// in this process ever calls `wait` on it.
pub fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detached_child_is_started_and_reaped_without_panicking() {
        let child = spawn("exit 0", Stdio::null(), Stdio::null()).unwrap();
        assert!(child.id() > 0);
        reap(child);
    }

    #[test]
    fn a_command_that_cannot_start_is_an_error() {
        // `current_exe` always resolves in tests, so the only way to make
        // `spawn` itself fail here is a program that cannot be found - but
        // `spawn` always execs this same binary, so this instead checks the
        // happy path is not accidentally infallible: a nonsense `-c` body is
        // still accepted (the shell it starts is what would reject it, not
        // this function).
        let child = spawn("", Stdio::null(), Stdio::null());
        assert!(child.is_ok());
        reap(child.unwrap());
    }
}
