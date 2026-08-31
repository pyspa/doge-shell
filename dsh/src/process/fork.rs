use crate::process::io::cloexec_pipe;
use anyhow::{Context as _, Result};
use nix::unistd::{ForkResult, Pid, fork, getpid, setpgid};
use tracing::debug;

use super::builtin::BuiltinProcess;
use super::process::Process;
use super::pty::PtyChildConfig;
use crate::shell::Shell;
use dsh_types::Context;
use libc::{STDERR_FILENO, STDOUT_FILENO};
use std::os::fd::IntoRawFd;

pub(crate) fn fork_builtin_process(
    ctx: &mut Context,
    process: &mut BuiltinProcess,
    shell: &mut Shell,
) -> Result<Pid> {
    debug!("fork_builtin_process for background execution");

    debug!(
        "🍴 BUILTIN: About to fork builtin process: {}",
        process.name
    );
    let pid = unsafe { fork().context("failed fork for builtin")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!(
                "🍴 BUILTIN: Parent process - forked builtin {} with child pid {}",
                process.name, child
            );
            Ok(child)
        }
        ForkResult::Child => {
            // Child process: execute builtin command
            // SAFETY: Avoid accessing any locks (like tracing/malloc) after fork in multi-threaded env
            let pid = getpid();
            // setpgid is a syscall, safe enough
            if let Err(_e) = setpgid(pid, pid) {
                // Silently fail or use raw stderr write if absolutely needed.
                // For now, minimizing risk by suppressing complex logging.
            }

            // Execute the builtin command
            // Note: process.launch might still use tracing internally if not careful.
            // Ideally builtins should be careful too, but at least we removed the immediate logging.
            if let Err(_e) = process.launch_sync(ctx, shell) {
                std::process::exit(1);
            }

            // Builtin commands complete immediately, so exit with success
            std::process::exit(0);
        }
    }
}

pub(crate) fn fork_process(
    ctx: &Context,
    job_pgid: Option<Pid>,
    process: &mut Process,
    shell: &mut Shell,
    pty: Option<PtyChildConfig>,
) -> Result<Pid> {
    debug!("🍴 FORK: Starting fork_process");
    debug!(
        "🍴 FORK: pgid: {:?}, foreground: {}",
        job_pgid, ctx.foreground
    );
    debug!(
        "🍴 FORK: Process I/O before capture - stdin={}, stdout={}, stderr={}",
        process.stdin, process.stdout, process.stderr
    );
    debug!(
        "🍴 FORK: Context I/O - infile={}, outfile={}, errfile={}",
        ctx.infile, ctx.outfile, ctx.errfile
    );

    // capture
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground && pty.is_none() {
        debug!("🍴 FORK: Creating capture pipe for stdout (background process)");
        let (pout, pin) = cloexec_pipe().context("failed pipe")?;
        process.stdout = pin.into_raw_fd();
        let pout_raw = pout.into_raw_fd();
        process.cap_stdout = Some(pout_raw);
        debug!(
            "🍴 FORK: Created capture pipe for stdout: read={}, write={}",
            pout_raw, process.stdout
        );
    } else {
        debug!(
            "🍴 FORK: No capture pipe needed for stdout (ctx.outfile={}, foreground={})",
            ctx.outfile, ctx.foreground
        );
    }

    if ctx.errfile == STDERR_FILENO && !ctx.foreground && pty.is_none() {
        debug!("🍴 FORK: Creating capture pipe for stderr (background process)");
        let (pout, pin) = cloexec_pipe().context("failed pipe")?;
        process.stderr = pin.into_raw_fd();
        let pout_raw = pout.into_raw_fd();
        process.cap_stderr = Some(pout_raw);
        debug!(
            "🍴 FORK: Created capture pipe for stderr: read={}, write={}",
            pout_raw, process.stderr
        );
    } else {
        debug!(
            "🍴 FORK: No capture pipe needed for stderr (ctx.errfile={}, foreground={})",
            ctx.errfile, ctx.foreground
        );
    }

    debug!(
        "🍴 FORK: Final process I/O - stdin={}, stdout={}, stderr={}",
        process.stdin, process.stdout, process.stderr
    );

    debug!("🍴 FORK: About to fork external process");

    // Resolve the program here rather than while the line was parsed: by now
    // every earlier command on the line has run, so this sees the directory and
    // the `PATH` the command is actually about to run with.
    let not_found = resolve_program(process, shell);
    // Where the diagnostic goes if it cannot: the descriptor this command was
    // given, not the shell's own, so `typo 2>/dev/null` is quiet.
    let not_found_fd = process.stderr;

    // Prepare execution data BEFORE forking to avoid allocation/locks in child
    let prepared = process.prepare_execution(shell.environment.clone())?;

    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            debug!("🍴 FORK: Parent process - child pid: {}", child);
            debug!("🍴 FORK: Parent process continuing with child management");
            // if process.stdout != STDOUT_FILENO {
            //     close(process.stdout).context("failed close")?;
            // }
            Ok(child)
        }
        ForkResult::Child => {
            // This is the child process
            // SAFETY: Avoid accessing any locks (like tracing/malloc) after fork in multi-threaded env

            // An unresolved command is a command that fails, the way every
            // other shell reports it: the message goes to *this* process's
            // stderr, so `typo 2>/dev/null` is quiet, and 127 is a status the
            // rest of the line can branch on.
            if let Some(message) = not_found {
                unsafe {
                    libc::write(
                        not_found_fd,
                        message.as_ptr() as *const libc::c_void,
                        message.len(),
                    );
                    libc::_exit(127);
                }
            }

            let pid = getpid();
            let pgid = job_pgid.unwrap_or(pid);

            if let Err(_e) =
                process.launch_prepared(pid, pgid, ctx.interactive, ctx.foreground, prepared, pty)
            {
                // Raw write to stderr or simple exit
                std::process::exit(1);
            }
            // When execv succeeds, it replaces with new program; when it fails, it exits, so this point is never reached
            // Explicit exit as a safety measure just in case
            std::process::exit(1);
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
