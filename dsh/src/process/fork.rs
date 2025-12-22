use anyhow::{Context as _, Result};
use nix::unistd::{ForkResult, Pid, fork, getpid, setpgid};
use tracing::{debug, error};

use super::builtin::BuiltinProcess;
use super::process::Process;
use crate::shell::Shell;
use dsh_types::Context;
use libc::{STDERR_FILENO, STDOUT_FILENO};
use nix::unistd::pipe;

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
            let pid = getpid();
            debug!(
                "🍴 BUILTIN: Child process - executing builtin command {} with pid {}",
                process.name, pid
            );
            debug!(
                "🍴 BUILTIN: Child process I/O - stdin={}, stdout={}, stderr={}",
                process.stdin, process.stdout, process.stderr
            );

            // Set process group for job control
            if let Err(e) = setpgid(pid, pid) {
                error!("Failed to setpgid for builtin: {}", e);
            }

            // Execute the builtin command
            if let Err(e) = process.launch(ctx, shell) {
                error!("Failed to launch builtin process: {}", e);
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
    pty_slave: Option<std::os::unix::io::RawFd>,
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
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground && pty_slave.is_none() {
        debug!("🍴 FORK: Creating capture pipe for stdout (background process)");
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stdout = pin;
        process.cap_stdout = Some(pout);
        debug!(
            "🍴 FORK: Created capture pipe for stdout: read={}, write={}",
            pout, pin
        );
    } else {
        debug!(
            "🍴 FORK: No capture pipe needed for stdout (ctx.outfile={}, foreground={})",
            ctx.outfile, ctx.foreground
        );
    }

    if ctx.errfile == STDERR_FILENO && !ctx.foreground && pty_slave.is_none() {
        debug!("🍴 FORK: Creating capture pipe for stderr (background process)");
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stderr = pin;
        process.cap_stderr = Some(pout);
        debug!(
            "🍴 FORK: Created capture pipe for stderr: read={}, write={}",
            pout, pin
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
            let pid = getpid();
            let pgid = job_pgid.unwrap_or(pid);
            debug!("🍴 FORK: Child process - pid: {}, pgid: {}", pid, pgid);
            debug!("🍴 FORK: Child process about to launch");

            if let Err(e) = process.launch(
                pid,
                pgid,
                ctx.interactive,
                ctx.foreground,
                shell.environment.clone(),
                pty_slave,
            ) {
                error!("🍴 FORK: Child process launch failed: {}", e);
                std::process::exit(1);
            }
            // When execv succeeds, it replaces with new program; when it fails, it exits, so this point is never reached
            // Explicit exit as a safety measure just in case
            debug!("🍴 FORK: Child process launch completed unexpectedly, exiting");
            std::process::exit(1);
        }
    }
}
