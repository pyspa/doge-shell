use super::async_io::{AsyncPtyMasterWriter, AsyncStdin, NonBlockingFdGuard};
use super::job::Job;
use super::job_process::JobProcess;
use super::pty::Pty;
use super::state::ProcessState;
use crate::process::job_wait::wait_job;
use crate::shell::Shell;
use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_types::Context;
use libc::STDIN_FILENO;
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use tokio::io::unix::AsyncFd;
use tracing::{debug, error};

pub async fn setup_pty(job: &mut Job, ctx: &mut Context) -> Result<Option<RawFd>> {
    if !(ctx.foreground
        && ctx.interactive
        && std::env::var("DSH_NO_PTY").is_err()
        && !job.disable_pty)
    {
        return Ok(None);
    }

    match Pty::new() {
        Ok(pty) => {
            debug!("PTY created: {:?}", pty);
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                let _ = pty.resize(rows, cols);
            }

            match pty.try_clone() {
                Ok(pty_clone) => {
                    let master_fd = pty_clone.master.into_raw_fd();
                    let mut monitor = crate::process::io::PtyMonitor::new(master_fd)?;

                    let output_task = tokio::spawn(async move {
                        monitor.process_output().await?;
                        Ok(String::from_utf8_lossy(&monitor.captured_output).to_string())
                    });
                    job.pty_output_task = Some(output_task);

                    if ctx.interactive {
                        let is_builtin = job
                            .process
                            .as_ref()
                            .map(|p| matches!(**p, JobProcess::Builtin(_)))
                            .unwrap_or(false);

                        if !is_builtin {
                            setup_pty_input_proxy(job, pty.try_clone()?).await;
                        }
                    }
                }
                Err(e) => error!("Failed to clone PTY for output: {}", e),
            }

            let slave_fd = pty.slave.as_raw_fd();
            job.pty = Some(pty);
            Ok(Some(slave_fd))
        }
        Err(e) => {
            error!(
                "Failed to create PTY: {}, falling back to normal execution",
                e
            );
            Ok(None)
        }
    }
}

pub async fn setup_pty_input_proxy(job: &mut Job, pty_in: Pty) {
    match AsyncPtyMasterWriter::new(pty_in.master.into_raw_fd()) {
        Ok(mut master_write) => {
            let input_task = tokio::spawn(async move {
                let _nonblock = NonBlockingFdGuard::new(STDIN_FILENO);
                match AsyncFd::new(STDIN_FILENO) {
                    Ok(fd) => {
                        let mut async_stdin: AsyncStdin = AsyncStdin { inner: fd };
                        let _ = tokio::io::copy(&mut async_stdin, &mut master_write).await;
                    }
                    Err(_) => {
                        let mut std_stdin = tokio::io::stdin();
                        let _ = tokio::io::copy(&mut std_stdin, &mut master_write).await;
                    }
                }
            });
            job.pty_input_task = Some(input_task);
        }
        Err(e) => error!("Failed to create AsyncPtyMasterWriter: {}", e),
    }
}

pub async fn cleanup_pty_tasks(job: &mut Job) {
    if let Some(input_task) = job.pty_input_task.take() {
        input_task.abort();
        let _ = input_task.await;
    }
    if let Some(output_task) = job.pty_output_task.take() {
        output_task.abort();
    }
}

pub async fn manage_execution(job: &mut Job, ctx: &mut Context) -> Result<()> {
    if !ctx.interactive {
        debug!(
            "JOB_LAUNCH_NON_INTERACTIVE: Non-interactive mode, waiting for job {} completion",
            job.job_id
        );
        // Note: wait_job call needs to be dispatched appropriately
        // Since wait_job is now possibly in job_wait.rs (not yet created) or still in job.rs
        // We will assume for now it is available via job instance or moved to helper.
        // But since we are MOVING logic out of job.rs, we should call the free function version if available.
        // For now, let's assume we call a method on Job, or a helper function.
        // To break circular dependency, this should likely call `wait_job(job, false)`.
        wait_job(job, false).await?;
    } else if ctx.foreground {
        if ctx.process_count > 0 {
            let raw_mode_enabled = if job.pty.is_some() {
                enable_raw_mode().is_ok()
            } else {
                false
            };

            // Similarly, put_in_foreground will be moved or refactored.
            // Assuming it's available on Job or as a helper.
            let res = crate::process::job_wait::put_in_foreground(job, false, false).await;
            if raw_mode_enabled {
                let _ = disable_raw_mode();
            }
            res?;
        }
    } else {
        crate::process::job_wait::put_in_background(job).await?;
    }
    Ok(())
}

pub async fn capture_output_and_history(
    job: &mut Job,
    ctx: &Context,
    shell: &mut Shell,
) -> Result<()> {
    let mut stdout_cap = String::new();
    let mut stderr_cap = String::new();

    job.pty = None;

    if let Some(output_task) = job.pty_output_task.take() {
        match output_task.await {
            Ok(Ok(output)) => stdout_cap = output,
            Ok(Err(e)) => error!("PTY output task failed: {}", e),
            Err(e) => error!("PTY output task join error: {}", e),
        }

        if let Some(input_task) = job.pty_input_task.take() {
            input_task.abort();
            let _ = input_task.await;
        }
    } else {
        let mut monitors_iter = job.monitors.iter();
        if let Some((Some(_), _)) = job.process.as_ref().map(|p| p.get_cap_out())
            && let Some(m) = monitors_iter.next()
        {
            stdout_cap = m.captured_output.clone();
        }
        if let Some((_, Some(_))) = job.process.as_ref().map(|p| p.get_cap_out())
            && let Some(m) = monitors_iter.next()
        {
            stderr_cap = m.captured_output.clone();
        }
    }

    if (!stdout_cap.is_empty() || !stderr_cap.is_empty()) && ctx.foreground {
        use dsh_types::output_history::OutputEntry;
        let stdout_stripped = console::strip_ansi_codes(&stdout_cap).to_string();
        let stderr_stripped = console::strip_ansi_codes(&stderr_cap).to_string();

        let exit_code = match job.state {
            ProcessState::Completed(c, _) => c as i32,
            _ => 0,
        };

        let entry = OutputEntry::new(job.cmd.clone(), stdout_stripped, stderr_stripped, exit_code);
        shell.environment.write().output_history.push(entry);
    }
    Ok(())
}
