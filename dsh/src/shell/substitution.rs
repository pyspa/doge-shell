//! Deferred substitution bodies: authorize-then-run for `$(...)` and `<(...)`.
//!
//! Pipes and forks happen here, but only after gating selected the job and
//! the guard approved the nested body. Timing moved; the fork/FD mechanics
//! themselves are unchanged (a follow-up owns that cleanup).

use super::authorize::{AuthorizationCancelled, AuthorizationDecision};
use super::authorize::{ConfirmFn, authorize_job_with};
use super::materialize::materialize_job;
use super::plan::ExecutionPlan;
use crate::process::{Job, ListOp};
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use std::future::Future;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd};
use std::pin::Pin;
use tracing::debug;

/// Run a command-substitution body through materialize/authorize/launch and
/// capture its stdout, with cwd and shell variables restored afterwards.
pub fn capture_subshell_plan_stdout<'a>(
    shell: &'a mut Shell,
    parent_ctx: &'a Context,
    plan: &'a ExecutionPlan,
    confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
    Box::pin(async move {
        use crate::process::io::cloexec_pipe;
        use dsh_builtin::shell_capabilities::ShellNavigation;
        use nix::unistd::close;
        use std::fs::File;
        use std::io::Read;

        let (read_end, write_end) = cloexec_pipe().context("failed to create substitution pipe")?;
        let read_fd = read_end.into_raw_fd();
        let write_fd = write_end.into_raw_fd();
        let reader = std::thread::spawn(move || {
            let mut file = unsafe { File::from_raw_fd(read_fd) };
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map(|_| buf)
        });

        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
        let entry_dir = std::env::current_dir().ok();
        let entry_vars = {
            let env = shell.environment.read();
            (
                env.variable_state.variables.clone(),
                env.variable_state.exported_vars.clone(),
            )
        };

        let mut launch_result: Result<()> = Ok(());
        let mut last_exit_code = 0_i32;
        let mut gate_op = ListOp::None;
        for planned in &plan.jobs {
            let next_gate = planned.list_op.clone();
            let should_run = match gate_op {
                ListOp::None => true,
                ListOp::And => last_exit_code == 0,
                ListOp::Or => last_exit_code != 0,
            };
            gate_op = next_gate;
            if !should_run {
                continue;
            }
            let materialized = materialize_job(shell, parent_ctx, planned, confirm).await?;
            let Some(materialized) = materialized else {
                continue;
            };
            match authorize_job_with(
                shell,
                &materialized.job,
                materialized.had_dynamic_expansion,
                confirm,
            )? {
                AuthorizationDecision::Allow => {}
                AuthorizationDecision::Deny => {
                    launch_result = Err(anyhow::anyhow!(AuthorizationCancelled));
                    break;
                }
            }
            let mut job = materialized.job;
            let mut job_ctx = parent_ctx.clone();
            job_ctx.outfile = libc::STDOUT_FILENO;
            job_ctx.captured_out = Some(write_fd);
            job_ctx.foreground = true;
            job_ctx.pid = None;
            job_ctx.pgid = None;
            job_ctx.process_count = 0;
            job.disable_pty = true;
            job.foreground = true;
            match job.launch(&mut job_ctx, shell).await {
                Ok(state @ crate::process::ProcessState::Completed(_, _)) => {
                    last_exit_code = state
                        .shell_exit_code()
                        .expect("completed state has exit code");
                    debug!("subshell job '{}' finished", job.cmd);
                }
                Ok(_) => {}
                Err(err) => {
                    launch_result = Err(err);
                    break;
                }
            }
        }

        if let Some(entry_dir) = entry_dir
            && std::env::current_dir().is_ok_and(|current| current != entry_dir)
            && let Err(err) = shell.changepwd(&entry_dir.to_string_lossy())
        {
            debug!("failed to restore directory after subshell: {}", err);
        }
        {
            let mut env = shell.environment.write();
            env.variable_state.variables = entry_vars.0;
            env.variable_state.exported_vars = entry_vars.1;
            env.refresh_derived_state("PATH");
            env.refresh_derived_state("Z_EXCLUDE");
        }
        if was_raw {
            let _ = crossterm::terminal::enable_raw_mode();
        }
        let _ = close(write_fd);
        let bytes = reader
            .join()
            .map_err(|_| anyhow::anyhow!("command substitution reader thread panicked"))?
            .context("failed to read command substitution output")?;
        launch_result?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    })
}

/// Authorize a `<(...)` body first, then start its producer and hand back
/// `/dev/fd/N`. No pipe and no fork happen before the guard approves.
pub fn start_process_substitution<'a>(
    shell: &'a mut Shell,
    parent_ctx: &'a Context,
    plan: &'a ExecutionPlan,
    confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
    Box::pin(async move {
        let mut concrete: Vec<Job> = Vec::new();
        // The producer body runs sequentially in `launch_subshell`, which has no
        // `&&`/`||` gating of its own; keep materialization in source order so
        // timing (not the fork mechanics) is what this task changes.
        for planned in &plan.jobs {
            let Some(materialized) = materialize_job(shell, parent_ctx, planned, confirm).await?
            else {
                continue;
            };
            match authorize_job_with(
                shell,
                &materialized.job,
                materialized.had_dynamic_expansion,
                confirm,
            )? {
                AuthorizationDecision::Allow => concrete.push(materialized.job),
                AuthorizationDecision::Deny => {
                    return Err(anyhow::anyhow!(AuthorizationCancelled));
                }
            }
        }
        if concrete.is_empty() {
            return Ok("/dev/null".to_string());
        }

        let tmode = match nix::sys::termios::tcgetattr(unsafe { BorrowedFd::borrow_raw(0) }) {
            Ok(mode) => Some(mode),
            Err(err) => {
                debug!("tcgetattr fallback for process substitution: {}", err);
                Context::new_safe(shell.pid, shell.pgid, false).shell_tmode
            }
        };
        let mut ctx = Context::new(shell.pid, shell.pgid, tmode, false);
        ctx.foreground = true;
        let (pout, pin) = nix::unistd::pipe().context("failed pipe")?;
        ctx.outfile = pin.as_raw_fd();
        crate::shell::eval::launch_subshell(shell, &mut ctx, concrete)?;
        drop(pin);
        Ok(format!("/dev/fd/{}", pout.into_raw_fd()))
    })
}
