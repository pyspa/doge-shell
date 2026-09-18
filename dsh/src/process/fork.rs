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
use crate::process::io::cloexec_pipe;
use anyhow::{Context as _, Result};
use nix::unistd::{ForkResult, Pid, fork};
use std::os::fd::{BorrowedFd, IntoRawFd};
use tracing::debug;

use super::process::Process;
use super::pty::{PtyChildConfig, PtyMode};
use crate::shell::Shell;
use dsh_types::Context;
use libc::{STDERR_FILENO, STDOUT_FILENO};

pub(crate) fn fork_process(
    ctx: &Context,
    job_pgid: Option<Pid>,
    process: &mut Process,
    shell: &mut Shell,
    pty: Option<PtyChildConfig>,
) -> Result<Pid> {
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

    // capture
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground && pty.is_none() {
        debug!("FORK: Creating capture pipe for stdout (background process)");
        let (pout, pin) = cloexec_pipe().context("failed pipe")?;
        process.stdout = pin.into_raw_fd();
        let pout_raw = pout.into_raw_fd();
        process.cap_stdout = Some(pout_raw);
        debug!(
            "FORK: Created capture pipe for stdout: read={}, write={}",
            pout_raw, process.stdout
        );
    } else {
        debug!(
            "FORK: No capture pipe needed for stdout (ctx.outfile={}, foreground={})",
            ctx.outfile, ctx.foreground
        );
    }

    if ctx.errfile == STDERR_FILENO && !ctx.foreground && pty.is_none() {
        debug!("FORK: Creating capture pipe for stderr (background process)");
        let (pout, pin) = cloexec_pipe().context("failed pipe")?;
        process.stderr = pin.into_raw_fd();
        let pout_raw = pout.into_raw_fd();
        process.cap_stderr = Some(pout_raw);
        debug!(
            "FORK: Created capture pipe for stderr: read={}, write={}",
            pout_raw, process.stderr
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
    let bundle = process
        .prepare_execution(shell.environment.clone())?
        .into_bundle();

    // Exec-error pipe: CLOEXEC write end closes on `execve` success (parent
    // sees EOF); the child writes one `ChildExecError` record on failure.
    let (err_read, err_write) = cloexec_pipe().context("failed exec-error pipe")?;
    let err_read_fd = err_read.into_raw_fd();
    let err_write_fd = err_write.into_raw_fd();

    let full_proxy_pty = pty.is_some_and(|pty| pty.mode == PtyMode::FullProxy);
    let pty_slave = pty.map(|pty| pty.slave).unwrap_or(-1);
    // `getpid` in the child decides the default pgid; pass -1 for "none".
    let pgid_raw = job_pgid.map(Pid::as_raw).unwrap_or(-1);

    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!("FORK: Parent process - child pid: {}", child);
            // The write end must close here so EOF reliably means "exec'd".
            unsafe { libc::close(err_write_fd) };
            drain_exec_error(err_read_fd, &process.cmd, process.stderr, &process.argv);
            unsafe { libc::close(err_read_fd) };
            Ok(child)
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
    if let Some(path) = shell.environment.read().lookup(&name) {
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
        .and_then(|cwd| dsh_builtin::task::list_tasks_in_dir(&cwd).ok())
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
