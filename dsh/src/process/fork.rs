//! Parent-side spawn preparation for external commands.
//!
//! The parent resolves the program, builds the `execve` image (strings plus
//! pointer arrays), creates the exec-error pipe, and forks. The child branch
//! does nothing but call `super::child_exec::exec_external_child` — one tiny
//! raw-syscall function that ends in `execve` or `_exit`. In particular the
//! child performs no `tracing`, no `anyhow`, no allocation, no locks, and no
//! `std::process::exit`.
//!
//! Failure diagnostics (`EACCES`, `dup2` errors, ...) are formatted here in
//! the parent, which owns allocation and `format!`, and written to the
//! process's stderr target fd.

use crate::process::child_exec::{
    ChildExecError, RawChildPlan, STAGE_CLOSE, STAGE_DUP2_STDERR, STAGE_DUP2_STDIN,
    STAGE_DUP2_STDOUT, STAGE_EXECVE, STAGE_SETPGID, STAGE_SETSID, STAGE_SIGNAL,
    exec_external_child,
};
use crate::process::io::{OutputMonitor, cloexec_pipe};
use anyhow::{Context as _, Result};
use nix::unistd::{ForkResult, Pid, close, fork};
use std::os::fd::{BorrowedFd, IntoRawFd, RawFd};
use tracing::debug;

use super::process::Process;
use super::pty::{PtyChildConfig, PtyMode};
use crate::shell::Shell;
use dsh_types::Context;
use dsh_types::observed_output::ObservedStream;
use libc::{STDERR_FILENO, STDOUT_FILENO};

pub(crate) fn fork_process(
    ctx: &Context,
    job_pgid: Option<Pid>,
    process: &mut Process,
    shell: &mut Shell,
    pty: Option<PtyChildConfig>,
) -> Result<(Pid, Vec<OutputMonitor>)> {
    debug!("FORK: Starting fork_process");
    debug!("FORK: pgid: {:?}, foreground: {}", job_pgid, ctx.foreground);
    debug!(
        "FORK: Process I/O before capture - stdin={}, stdout={}, stderr={}",
        process.stdin, process.stdout, process.stderr
    );
    debug!(
        "FORK: Context I/O - infile={}, outfile={}, errfile={}",
        ctx.infile, ctx.outfile, ctx.errfile
    );

    // Capture monitors are built before the fork: construction failure drops
    // both pipe ends via RAII with no child spawned. The write ends move
    // into the child wiring; the monitors travel back to the caller for the
    // single move into `Job.monitors` once the child exists.
    let mut monitors = Vec::new();
    // Write ends installed on `process` below; closed here when a pre-fork
    // step fails after their creation.
    let mut created_writes: Vec<RawFd> = Vec::new();
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground && pty.is_none() {
        debug!("FORK: Creating capture pipe for stdout (background process)");
        let (read, write) = cloexec_pipe().context("failed pipe")?;
        let monitor =
            OutputMonitor::new(read, ctx.output_observer.clone(), ObservedStream::Stdout)?;
        let write_fd = write.into_raw_fd();
        process.stdout = write_fd;
        created_writes.push(write_fd);
        monitors.push(monitor);
        debug!(
            "FORK: Created capture pipe for stdout: write={}",
            process.stdout
        );
    } else {
        debug!(
            "FORK: No capture pipe needed for stdout (ctx.outfile={}, foreground={})",
            ctx.outfile, ctx.foreground
        );
    }

    if ctx.errfile == STDERR_FILENO && !ctx.foreground && pty.is_none() {
        debug!("FORK: Creating capture pipe for stderr (background process)");
        let (read, write) = cloexec_pipe().context("failed pipe")?;
        let monitor =
            OutputMonitor::new(read, ctx.output_observer.clone(), ObservedStream::Stderr)?;
        let write_fd = write.into_raw_fd();
        process.stderr = write_fd;
        created_writes.push(write_fd);
        monitors.push(monitor);
        debug!(
            "FORK: Created capture pipe for stderr: write={}",
            process.stderr
        );
    } else {
        debug!(
            "FORK: No capture pipe needed for stderr (ctx.errfile={}, foreground={})",
            ctx.errfile, ctx.foreground
        );
    }

    debug!(
        "FORK: Final process I/O - stdin={}, stdout={}, stderr={}",
        process.stdin, process.stdout, process.stderr
    );

    debug!("FORK: About to fork external process");

    // Resolve the program here rather than while the line was parsed: by now
    // every earlier command on the line has run, so this sees the directory and
    // the `PATH` the command is actually about to run with.
    let not_found = resolve_program(process, shell);
    // Where the diagnostic goes if it cannot: the descriptor this command was
    // given, not the shell's own, so `typo 2>/dev/null` is quiet.
    let not_found_fd = process.stderr;

    // Prepare execution data BEFORE forking, including the null-terminated
    // pointer arrays: the child only reads, never allocates.
    let bundle = match process.prepare_execution(shell.environment.clone()) {
        Ok(prepared) => prepared.into_bundle(),
        Err(err) => {
            for fd in created_writes {
                let _ = close(fd);
            }
            return Err(err);
        }
    };

    // Exec-error pipe: CLOEXEC write end closes on `execve` success (parent
    // sees EOF); the child writes one `ChildExecError` record on failure.
    let (err_read, err_write) = cloexec_pipe().context("failed exec-error pipe")?;
    let err_read_fd = err_read.into_raw_fd();
    let err_write_fd = err_write.into_raw_fd();

    let full_proxy_pty = pty.is_some_and(|pty| pty.mode == PtyMode::FullProxy);
    let pty_slave = pty.map(|pty| pty.slave).unwrap_or(-1);
    // setpgid(0, 0) makes the first child its own process-group leader.
    // Later pipeline stages receive the existing job PGID.
    let pgid_raw = job_pgid.map(Pid::as_raw).unwrap_or(0);

    let pid = match unsafe { fork().context("failed fork") } {
        Ok(pid) => pid,
        Err(err) => {
            unsafe { libc::close(err_read_fd) };
            unsafe { libc::close(err_write_fd) };
            for fd in created_writes {
                let _ = close(fd);
            }
            return Err(err);
        }
    };

    match pid {
        ForkResult::Parent { child } => {
            debug!("FORK: Parent process - child pid: {}", child);
            // The write end must close here so EOF reliably means "exec'd".
            unsafe { libc::close(err_write_fd) };
            drain_exec_error(err_read_fd, &process.cmd, process.stderr, &process.argv);
            unsafe { libc::close(err_read_fd) };
            Ok((child, monitors))
        }
        ForkResult::Child => {
            // The ONLY post-fork logic: raw syscalls, then execve/_exit.
            // No tracing, no anyhow, no allocation, no locks, no
            // `std::process::exit`.
            let (not_found_msg, not_found_len) = match &not_found {
                Some(message) => (message.as_ptr(), message.len()),
                None => (std::ptr::null(), 0),
            };
            let plan = RawChildPlan {
                executable: bundle.executable_ptr(),
                argv: bundle.argv_ptr(),
                envp: bundle.envp_ptr(),
                stdin: process.stdin,
                stdout: process.stdout,
                stderr: process.stderr,
                pgid: pgid_raw,
                interactive: ctx.interactive,
                full_proxy_pty,
                pty_slave,
                exec_error_fd: err_write_fd,
                not_found_msg,
                not_found_len,
                not_found_fd,
            };
            unsafe { exec_external_child(&plan) }
        }
    }
}

/// Read the exec-error pipe to EOF. Success closes the write end via
/// `CLOEXEC` and yields no bytes; failure yields one `ChildExecError` whose
/// diagnostic the parent formats and writes to the process's stderr target.
fn drain_exec_error(err_read_fd: i32, cmd: &str, stderr_fd: i32, argv: &[String]) {
    // A single small record; a short read loop tolerates partial delivery.
    let mut record = ChildExecError { stage: 0, errno: 0 };
    let mut filled = 0usize;
    let size = std::mem::size_of::<ChildExecError>();
    while filled < size {
        let chunk = unsafe {
            libc::read(
                err_read_fd,
                (std::ptr::addr_of_mut!(record) as *mut u8).add(filled) as *mut libc::c_void,
                size - filled,
            )
        };
        if chunk <= 0 {
            break;
        }
        filled += chunk as usize;
    }
    if filled == 0 {
        // EOF: the child exec'd and the write end closed.
        return;
    }
    if filled != size {
        write_process_stderr(
            stderr_fd,
            format!("dsh: {cmd}: failed to start (short exec-error report)\r\n").as_bytes(),
        );
        return;
    }
    let detail = std::io::Error::from_raw_os_error(record.errno).to_string();
    let what = match record.stage {
        STAGE_SETPGID => "failed to join process group",
        STAGE_SETSID => "failed to create session",
        STAGE_SIGNAL => "failed to reset signal handlers",
        STAGE_DUP2_STDIN => "failed to set up stdin",
        STAGE_DUP2_STDOUT => "failed to set up stdout",
        STAGE_DUP2_STDERR => "failed to set up stderr",
        STAGE_CLOSE => "failed to close file descriptor",
        STAGE_EXECVE => {
            // Keep the historical hint for the most common case.
            let _ = argv;
            if record.errno == libc::EACCES {
                write_process_stderr(
                    stderr_fd,
                    format!("dsh: {cmd}: Permission denied ({detail}). chmod(1) may help.\r\n")
                        .as_bytes(),
                );
                return;
            }
            "failed to execute"
        }
        _ => "failed to start",
    };
    write_process_stderr(
        stderr_fd,
        format!("dsh: {cmd}: {what}: {detail}\r\n").as_bytes(),
    );
}

pub(crate) fn write_process_stderr(fd: i32, mut bytes: &[u8]) {
    if fd < 0 {
        return;
    }
    while !bytes.is_empty() {
        let fd_ref = unsafe { BorrowedFd::borrow_raw(fd) };
        match nix::unistd::write(fd_ref, bytes) {
            Ok(0) => break,
            Ok(n) => bytes = &bytes[n..],
            Err(_) => break,
        }
    }
}

/// Point `process.cmd` at the program to execute, or describe why it cannot be.
///
/// Returns the message the child should print before exiting 127. Everything
/// that needs the shell's state — the command-not-found hooks and the
/// "did you mean" list — happens here, in the parent, because the child cannot
/// safely take a lock after `fork`.
fn resolve_program(process: &mut Process, shell: &mut Shell) -> Option<Vec<u8>> {
    let name = process.cmd.clone();
    // Command-scoped `PATH=...` selects the lookup PATH (last wins, matching
    // `prepare_execution`); slash names still bypass it as explicit pathnames.
    let path_override = process.path_override();
    if let Some(path) = shell
        .environment
        .read()
        .lookup_with_path_override(&name, path_override)
    {
        process.cmd = path;
        return None;
    }

    shell.exec_command_not_found_hooks(&name);

    let mut message = format!("dsh: {name}: command not found\r\n");

    let paths = shell.environment.read().variable_state.paths.clone();
    let builtins: Vec<String> = dsh_builtin::get_all_commands()
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();
    let suggestions = crate::command_suggestion::find_similar_commands(&name, &paths, &builtins);
    if let Some(suggestion_msg) = crate::command_suggestion::format_suggestions(&suggestions) {
        message.push_str(&suggestion_msg);
    }

    let task_suggestions = std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            // Same shell runtime the command runs with: logical PATH plus
            // exported child environment. The lock is released before any
            // task filesystem scan or provider subprocess runs.
            let runtime = {
                let env = shell.environment.read();
                dsh_builtin::task::TaskDiscoveryRuntime::new(
                    env.variable_state
                        .paths
                        .iter()
                        .map(std::path::PathBuf::from)
                        .collect(),
                    env.child_process_env(),
                )
            };
            dsh_builtin::task::list_tasks_in_dir(&cwd, &runtime).ok()
        })
        .map(|tasks| {
            let task_names: Vec<String> = tasks.into_iter().map(|task| task.name).collect();
            crate::command_suggestion::find_similar_candidates(&name, &task_names)
        })
        .unwrap_or_default();
    if !task_suggestions.is_empty() {
        let commands = task_suggestions
            .iter()
            .map(|suggestion| format!("task {}", suggestion.command))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!("\rProject tasks: {commands}\r\n"));
    }

    Some(message.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::shell::Shell;
    use nix::unistd::{getpgid, getpgrp, getpid};

    #[test]
    fn interactive_external_initial_child_becomes_process_group_leader() {
        // Bare `sleep` resolves via `PATH` in `resolve_program`, so no
        // absolute `/bin` vs `/usr/bin` literal is needed here (Linux and
        // macOS place it differently; `check-portability.py` flags those).
        let path = "sleep";
        // Force the exact buggy branch: interactive + non-FullProxy + first
        // child (`job_pgid == None`). `Context::new_safe` detects a pipe in
        // `cargo test`, so `interactive` must be forced on.
        let mut ctx = Context::new_safe(getpid(), getpgrp(), true);
        ctx.interactive = true;
        ctx.foreground = true;
        ctx.pgid = None;

        let env = Environment::new();
        let mut shell = Shell::new(env);
        let mut process = Process::new(path.to_string(), vec![path.to_string(), "30".to_string()]);

        let (child, monitors) =
            fork_process(&ctx, None, &mut process, &mut shell, None).expect("fork_process failed");
        assert!(monitors.is_empty());

        // `fork_process` drains the exec-error pipe, so return means the
        // child already passed `setpgid` + `execve`. No timing sleep needed.
        let observed = getpgid(Some(child));

        // Always clean up before asserting so a failure never leaves
        // `sleep 30` or a zombie behind. Only signal the group when the
        // child actually leads it; otherwise `killpg(child)` could address
        // an unrelated reused pgid. `sleep` spawns no children, so a
        // direct `kill(child)` suffices in the failure case.
        let leads_group = observed.as_ref().is_ok_and(|pgid| *pgid == child);
        if leads_group {
            let _ = nix::sys::signal::killpg(child, nix::sys::signal::Signal::SIGKILL);
        }
        let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
        let _ = nix::sys::wait::waitpid(child, None);

        let observed_pgid = observed.expect("getpgid(child) failed: child likely failed setpgid");
        assert_eq!(
            observed_pgid, child,
            "first interactive child must lead its own process group"
        );
    }
}
