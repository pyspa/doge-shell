//! Pipe-capture plumbing `eval_str` builds on for `|>` and `|:`.
//!
//! `execute_with_capture` stays in-process: it never forked, so no boundary
//! work applies to it. `$(...)` capture and `<(...)` producers live in
//! `super::super::substitution`, on the re-exec protocol shared with
//! background builtins (`crate::process::reexec`).
use crate::process::{Job, JobLaunchOutcome, ProcessState};
use crate::shell::Shell;
use anyhow::{Context as _, Result, anyhow};
use dsh_types::Context;
use tracing::debug;

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

    // A redirection setup failure is an ordinary command failure: route the
    // diagnostic through the capture stderr (never a hard-coded process
    // stderr, which would bypass `|>` / `|:`), then fall through to the
    // normal reader join so the caller sees `(1, stdout, stderr)`.
    if let Ok(JobLaunchOutcome::CommandFailed(failure)) = &launch_result {
        let _ = capture_ctx.write_stderr(&failure.message);
    }

    // Ensure writer ends are closed in parent so reader threads can finish.
    let _ = close(stdout_write_fd);
    let _ = close(stderr_write_fd);

    let stdout = join_pipe_reader(stdout_reader, "stdout")?;
    let stderr = join_pipe_reader(stderr_reader, "stderr")?;

    let state = match launch_result? {
        JobLaunchOutcome::Process(state) => state,
        JobLaunchOutcome::CommandFailed(failure) => {
            debug!(
                "Capture complete with command failure: exit={}",
                failure.exit_code
            );
            return Ok((failure.exit_code, stdout, stderr));
        }
    };
    let exit_code = match state {
        ProcessState::Completed(_, _) => state
            .shell_exit_code()
            .expect("completed state has exit code"),
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
