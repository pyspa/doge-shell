//! Running a job as a subshell rather than as part of the interactive job table:
//! `(...)`'s own subshell (`launch_subshell`/`spawn_subshell`, which does fork)
//! for `<(...)` producers, plus `execute_with_capture`'s pipe-capture plumbing
//! that `eval_str` builds on. Deferred `$(...)` bodies go through
//! `crate::shell::substitution::capture_subshell_plan_stdout`, which
//! authorizes each nested body before running it.
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
