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

const DSH_NO_PTY_ENV: &str = "DSH_NO_PTY";

#[derive(Debug)]
pub(crate) struct ForegroundPtyRawModeGuard {
    enabled: bool,
}

impl ForegroundPtyRawModeGuard {
    pub(crate) fn new(job: &Job, ctx: &Context) -> Self {
        if should_enable_foreground_pty_raw_mode(job, ctx) {
            match enable_raw_mode() {
                Ok(()) => Self { enabled: true },
                Err(err) => {
                    error!("Failed to enable raw mode for PTY job: {}", err);
                    Self { enabled: false }
                }
            }
        } else {
            Self { enabled: false }
        }
    }
}

impl Drop for ForegroundPtyRawModeGuard {
    fn drop(&mut self) {
        if self.enabled
            && let Err(err) = disable_raw_mode()
        {
            error!("Failed to disable raw mode after PTY job: {}", err);
        }
    }
}

pub(crate) fn should_create_pty(ctx: &Context, disable_pty: bool, no_pty_env: bool) -> bool {
    ctx.foreground && ctx.interactive && !disable_pty && !no_pty_env
}

fn is_builtin_job(job: &Job) -> bool {
    job.process
        .as_ref()
        .map(|p| matches!(**p, JobProcess::Builtin(_)))
        .unwrap_or(false)
}

fn should_enable_foreground_pty_raw_mode(job: &Job, ctx: &Context) -> bool {
    ctx.foreground && ctx.interactive && job.pty.is_some() && !is_builtin_job(job)
}

pub async fn setup_pty(job: &mut Job, ctx: &mut Context) -> Result<Option<RawFd>> {
    if !should_create_pty(ctx, job.disable_pty, std::env::var(DSH_NO_PTY_ENV).is_ok()) {
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
                    let mut monitor = crate::process::io::PtyMonitor::new(
                        master_fd,
                        ctx.output_observer.clone(),
                    )?;

                    let output_task = tokio::spawn(async move {
                        monitor.process_output().await?;
                        Ok(String::from_utf8_lossy(&monitor.captured_output).to_string())
                    });
                    job.pty_output_task = Some(output_task);

                    if ctx.interactive && !is_builtin_job(job) {
                        setup_pty_input_proxy(job, pty.try_clone()?).await;
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
            // Similarly, put_in_foreground will be moved or refactored.
            // Assuming it's available on Job or as a helper.
            crate::process::job_wait::put_in_foreground(job, false, false).await?;
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

#[cfg(test)]
mod tests {
    use super::{is_builtin_job, should_create_pty, should_enable_foreground_pty_raw_mode};
    use crate::process::{BuiltinProcess, Job, JobProcess, Process, Pty};
    use dsh_types::Context;
    use dsh_types::ExitStatus;
    use dsh_types::terminal::{ShellMode, TerminalState};
    use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
    use nix::unistd::Pid;

    fn test_context(foreground: bool, interactive: bool) -> Context {
        Context {
            shell_pid: Pid::from_raw(1),
            shell_pgid: Pid::from_raw(1),
            shell_tmode: None,
            terminal_state: TerminalState::non_terminal(),
            shell_mode: if interactive {
                ShellMode::Interactive
            } else {
                ShellMode::Script
            },
            foreground,
            interactive,
            infile: STDIN_FILENO,
            outfile: STDOUT_FILENO,
            errfile: STDERR_FILENO,
            captured_out: None,
            output_observer: None,
            save_history: true,
            pid: None,
            pgid: None,
            process_count: 0,
        }
    }

    fn test_builtin(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(0)
    }

    fn test_job_with_process(process: JobProcess, with_pty: bool) -> Job {
        let mut job = Job::new("test".to_string(), Pid::from_raw(1));
        job.set_process(process);
        if with_pty {
            job.pty = Some(Pty::new().expect("failed to create test pty"));
        }
        job
    }

    #[test]
    fn should_create_pty_only_for_foreground_interactive_jobs() {
        let interactive_foreground = test_context(true, true);
        let interactive_background = test_context(false, true);
        let non_interactive_foreground = test_context(true, false);

        assert!(should_create_pty(&interactive_foreground, false, false));
        assert!(!should_create_pty(&interactive_background, false, false));
        assert!(!should_create_pty(
            &non_interactive_foreground,
            false,
            false
        ));
    }

    #[test]
    fn should_create_pty_respects_disable_flags() {
        let ctx = test_context(true, true);

        assert!(!should_create_pty(&ctx, true, false));
        assert!(!should_create_pty(&ctx, false, true));
    }

    #[test]
    fn detects_builtin_jobs() {
        let builtin_job = test_job_with_process(
            JobProcess::Builtin(BuiltinProcess::new(
                "aic".to_string(),
                test_builtin,
                vec!["aic".to_string()],
            )),
            false,
        );
        let command_job = test_job_with_process(
            JobProcess::Command(Process::new(
                "/bin/echo".to_string(),
                vec!["echo".to_string()],
            )),
            false,
        );

        assert!(is_builtin_job(&builtin_job));
        assert!(!is_builtin_job(&command_job));
    }

    #[test]
    fn foreground_builtin_pty_jobs_do_not_reenable_raw_mode() {
        let ctx = test_context(true, true);
        let builtin_job = test_job_with_process(
            JobProcess::Builtin(BuiltinProcess::new(
                "aic".to_string(),
                test_builtin,
                vec!["aic".to_string()],
            )),
            true,
        );
        let command_job = test_job_with_process(
            JobProcess::Command(Process::new(
                "/bin/echo".to_string(),
                vec!["echo".to_string()],
            )),
            true,
        );

        assert!(!should_enable_foreground_pty_raw_mode(&builtin_job, &ctx));
        assert!(should_enable_foreground_pty_raw_mode(&command_job, &ctx));
    }
}
