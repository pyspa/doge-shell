//! Running a job as a subshell rather than as part of the interactive job table: `$(...)`/backtick command substitution (`capture_subshell_stdout`, in-process so a multi-threaded Tokio
//! runtime never forks) and `(...)`'s own subshell (`launch_subshell`/`spawn_subshell`, which does fork), plus `execute_with_capture`'s pipe-capture plumbing that both `eval_str`
//! and `capture_subshell_stdout` build on.
use anyhow::Context as _;
use nix::unistd::{ForkResult, Pid, fork, getpid, setpgid};
use tokio::task;

use super::*;

/// Execute a job and capture its stdout and stderr
/// Returns (exit_code, stdout, stderr)
pub async fn execute_with_capture(
    shell: &mut Shell,
    ctx: &Context,
    job: &mut Job,
) -> Result<(i32, String, String)> {
    use crate::process::io::cloexec_pipe;
    use libc::STDOUT_FILENO;
    use nix::unistd::close;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, IntoRawFd, RawFd};
    use std::thread;

    fn spawn_pipe_reader(fd: RawFd) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
        thread::spawn(move || {
            let mut file = unsafe { File::from_raw_fd(fd) };
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            Ok(buf)
        })
    }

    fn join_pipe_reader(
        handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
        name: &str,
    ) -> Result<String> {
        let bytes = handle
            .join()
            .map_err(|_| anyhow!("{} reader thread panicked", name))?
            .with_context(|| format!("Failed to read {} capture stream", name))?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    let (stdout_read, stdout_write) =
        cloexec_pipe().context("failed to create stdout capture pipe")?;
    let (stderr_read, stderr_write) =
        cloexec_pipe().context("failed to create stderr capture pipe")?;

    let stdout_read_fd = stdout_read.into_raw_fd();
    let stdout_write_fd = stdout_write.into_raw_fd();
    let stderr_read_fd = stderr_read.into_raw_fd();
    let stderr_write_fd = stderr_write.into_raw_fd();

    // Drain capture streams concurrently to avoid pipe-buffer deadlocks on large output.
    let stdout_reader = spawn_pipe_reader(stdout_read_fd);
    let stderr_reader = spawn_pipe_reader(stderr_read_fd);

    let mut capture_ctx = ctx.clone();
    capture_ctx.outfile = STDOUT_FILENO;
    capture_ctx.errfile = stderr_write_fd;
    capture_ctx.captured_out = Some(stdout_write_fd);
    capture_ctx.pid = None;
    capture_ctx.pgid = None;
    capture_ctx.process_count = 0;
    capture_ctx.foreground = true;

    let original_disable_pty = job.disable_pty;
    let original_foreground = job.foreground;
    job.disable_pty = true;
    job.foreground = true;

    let launch_result = job.launch(&mut capture_ctx, shell).await;

    job.disable_pty = original_disable_pty;
    job.foreground = original_foreground;

    // Ensure writer ends are closed in parent so reader threads can finish.
    let _ = close(stdout_write_fd);
    let _ = close(stderr_write_fd);

    let stdout = join_pipe_reader(stdout_reader, "stdout")?;
    let stderr = join_pipe_reader(stderr_reader, "stderr")?;

    let state = launch_result?;
    let exit_code = match state {
        ProcessState::Completed(code, _) => i32::from(code),
        ProcessState::Stopped(_, _) => 130,
        ProcessState::Running => 0,
    };

    debug!(
        "Capture complete: exit={}, stdout={} bytes, stderr={} bytes",
        exit_code,
        stdout.len(),
        stderr.len()
    );

    Ok((exit_code, stdout, stderr))
}
pub fn launch_subshell(shell: &mut Shell, ctx: &mut Context, jobs: Vec<Job>) -> Result<()> {
    for mut job in jobs {
        disable_raw_mode().ok();
        let pid = task::block_in_place(|| {
            // Avoid nested-runtime panic by driving only this future directly.
            futures::executor::block_on(spawn_subshell(shell, ctx, &mut job))
        })?;
        debug!("spawned subshell cmd:{} pid: {:?}", job.cmd, pid);
        let res = wait_pid_job(pid, false);
        debug!("wait subshell exit:{:?}", res);
        enable_raw_mode().ok();
    }

    Ok(())
}
/// Run `jobs` in this process and return everything they wrote to stdout.
///
/// Command substitution used to go through `launch_subshell`, which forks. Two
/// things went wrong there: the fork happens under a multi-threaded Tokio
/// runtime, so the child aborted inside Tokio's IO driver as soon as it awaited
/// anything, and the substitution pipe was handed over as a bare `ctx.outfile`,
/// which the non-interactive auto-capture path in `JobProcess::launch` happily
/// overwrote — the result came back empty and the inner output leaked to the
/// terminal.
///
/// Both go away by staying in-process and using the same `captured_out` wiring
/// `execute_with_capture` relies on. The reader runs on its own thread so a
/// result larger than the pipe buffer cannot deadlock the job producing it, and
/// stderr is left alone so diagnostics still reach the terminal.
///
/// What fork used to give away for free — isolation — has to be paid for
/// explicitly. The working directory and the shell variables are snapshotted
/// and restored around the run. Not restored, because a subshell in one process
/// cannot have them: the job table, the Lisp environment, and a bare
/// `NAME=value` inside the substitution, which `parse_command` applies while it
/// is still *parsing* the outer line and so lands before this function is even
/// called.
pub fn capture_subshell_stdout(shell: &mut Shell, ctx: &Context, jobs: Vec<Job>) -> Result<String> {
    use crate::process::io::cloexec_pipe;
    use libc::STDOUT_FILENO;
    // `changepwd` is the single funnel every navigation goes through (cd, z,
    // bookmark, pushd, popd), so restoring through it keeps `OLDPWD`, the
    // directory stack and the chpwd hooks consistent.
    use dsh_builtin::shell_capabilities::ShellNavigation;
    use nix::unistd::close;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, IntoRawFd};

    let (read_end, write_end) = cloexec_pipe().context("failed to create substitution pipe")?;
    let read_fd = read_end.into_raw_fd();
    let write_fd = write_end.into_raw_fd();

    let reader = std::thread::spawn(move || {
        let mut file = unsafe { File::from_raw_fd(read_fd) };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map(|_| buf)
    });

    // The jobs run with their own stdio, so hand the terminal back for the
    // duration and restore whatever mode the REPL had set.
    let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if was_raw {
        disable_raw_mode().ok();
    }

    // Running in-process means a builtin inside the substitution writes to the
    // *shell's* state: `$(cd /tmp)` moved the whole session and `$(X=1)` left
    // the variable behind. Snapshot what a subshell is supposed to keep to
    // itself and put it back afterwards.
    let entry_dir = std::env::current_dir().ok();
    let entry_vars = {
        let environment = shell.environment.read();
        (
            environment.variable_state.variables.clone(),
            environment.variable_state.exported_vars.clone(),
        )
    };

    let mut launch_result = Ok(());
    let mut last_exit_code = 0_i32;
    // `list_op` lives on the *previous* job, so it gates the one after it.
    let mut gate_op = ListOp::None;
    for mut job in jobs {
        let next_gate_op = job.list_op.clone();
        let should_run = match gate_op {
            ListOp::None => true,
            ListOp::And => last_exit_code == 0,
            ListOp::Or => last_exit_code != 0,
        };
        gate_op = next_gate_op;
        if !should_run {
            continue;
        }

        let mut job_ctx = ctx.clone();
        job_ctx.outfile = STDOUT_FILENO;
        job_ctx.captured_out = Some(write_fd);
        job_ctx.foreground = true;
        job_ctx.pid = None;
        job_ctx.pgid = None;
        job_ctx.process_count = 0;
        job.disable_pty = true;
        job.foreground = true;

        launch_result = task::block_in_place(|| {
            // Avoid nested-runtime panic by driving only this future directly.
            futures::executor::block_on(job.launch(&mut job_ctx, shell))
        })
        .map(|state| {
            debug!("subshell job '{}' finished: {:?}", job.cmd, state);
            if let ProcessState::Completed(code, _) = state {
                last_exit_code = i32::from(code);
            }
        });

        if launch_result.is_err() {
            break;
        }
    }

    // Restore the directory first, so the `OLDPWD` that `changepwd` writes is
    // itself overwritten by the snapshot below.
    if let Some(entry_dir) = entry_dir
        && std::env::current_dir().is_ok_and(|current| current != entry_dir)
        && let Err(err) = shell.changepwd(&entry_dir.to_string_lossy())
    {
        debug!("failed to restore directory after subshell: {}", err);
    }
    {
        let mut environment = shell.environment.write();
        environment.variable_state.variables = entry_vars.0;
        environment.variable_state.exported_vars = entry_vars.1;
        // Putting the maps back by hand skips the setters, so anything derived
        // from a variable the substitution touched has to be rebuilt.
        environment.refresh_derived_state("PATH");
        environment.refresh_derived_state("Z_EXCLUDE");
    }

    if was_raw {
        enable_raw_mode().ok();
    }

    // Close the write end here or the reader never sees EOF.
    let _ = close(write_fd);

    let bytes = reader
        .join()
        .map_err(|_| anyhow!("command substitution reader thread panicked"))?
        .context("failed to read command substitution output")?;

    launch_result?;

    Ok(String::from_utf8_lossy(&bytes).to_string())
}
async fn spawn_subshell(shell: &mut Shell, ctx: &mut Context, job: &mut Job) -> Result<Pid> {
    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            let pid = child;
            debug!("subshell parent setpgid parent pid:{} pgid:{}", pid, pid);
            setpgid(pid, pid).context("failed setpgid")?;
            Ok(pid)
        }
        ForkResult::Child => {
            // Child process
            // SAFETY: Do NOT use tracing here. Unsafe after fork.
            let pid = getpid();
            // setpgid is syscall
            if setpgid(pid, pid).is_err() {
                // ignore or raw write
            }

            job.pgid = Some(pid);
            ctx.pgid = Some(pid);

            // Execute
            let res = job.launch(ctx, shell).await;

            if let Ok(ProcessState::Completed(exit, _)) = res {
                std::process::exit(i32::from(exit));
            } else {
                std::process::exit(-1);
            }
        }
    }
}
