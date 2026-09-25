//! Deferred substitution bodies: authorize-then-run for `$(...)`.
//!
//! Every body executes in a re-exec helper (a fresh `dogesh` process running
//! the structured plan through the shared gate → materialize → authorize →
//! launch evaluator), never in a `fork()` child and never in-process. The
//! parent only moves pipes: it hands the helper its stdout target and reads
//! the capture pipe.
//!
//! Process substitution (`<(...)` / `>(...)`) lives in
//! `super::process_substitution`; this module re-exports its resource types
//! so existing `substitution::` paths keep resolving.
//!
//! Skipped branches (`false && $(...)`) spawn nothing: gating is checked in
//! the parent before any pipe or helper exists, and the helper re-checks
//! inner `&&`/`||` itself.

use super::authorize::{AuthorizationCancelled, ConfirmFn};
use super::plan::ExecutionPlan;
use crate::process::reexec::{ChildStdio, PlanExecMode, PlanSignalPolicy, spawn_plan_helper};
use crate::process::{ProcessState, WaitPidObservation};
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use std::future::Future;
use std::os::fd::AsRawFd as _;
use std::pin::Pin;

pub use super::process_substitution::{
    ExecutionResources, ProcessSubstitution, ProcessSubstitutionHelper,
    ProcessSubstitutionRegistry, ProducerHandle, ProducerRegistry, reap_producers_blocking,
    start_process_substitution,
};

/// Snapshot the shell state one helper needs. The snapshot is authoritative;
/// per-job gating of the *outer* line already happened before this call.
fn helper_snapshot(shell: &Shell) -> crate::environment::child_snapshot::ChildShellSnapshot {
    crate::environment::child_snapshot::ChildShellSnapshot::capture(&shell.environment.read())
}

/// Captured `$(...)` output plus the helper's actual shell exit code.
///
/// Both travel together: the caller needs the stdout bytes for field
/// expansion *and* the status for no-command simple-command semantics
/// (`$(false)` is status 1, not status 0 with empty output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSubstitution {
    /// Raw helper stdout; the caller trims/splits per word context.
    pub stdout: String,
    /// Helper's shell exit code, via [`ProcessState::shell_exit_code`].
    pub exit_code: i32,
}

/// Run a command-substitution body in a helper and capture its stdout.
///
/// Returns the raw output plus the helper's actual shell exit code; the
/// caller trims trailing newlines per word context. Parent shell state
/// (cwd, variables, aliases, ...) is never touched — isolation comes from
/// the process boundary, not save/restore.
///
/// A nested denial inside the helper aborts the whole chain with
/// `AuthorizationCancelled` (reported on the status fd), exactly as if the
/// body had run in-process: the outer command never runs on empty output.
pub fn capture_subshell_plan_stdout<'a>(
    shell: &'a mut Shell,
    parent_ctx: &'a Context,
    plan: &'a ExecutionPlan,
    mode: PlanExecMode,
    _confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<CapturedSubstitution>> + 'a>> {
    Box::pin(async move {
        // The helper may interact with the terminal (`helper_confirm` reads
        // `/dev/tty`, nested bodies can run interactive commands), so run it
        // with raw mode paused, as the pre-re-exec engine did around its
        // substitution reads. No-op when raw mode is off (non-interactive
        // runs, unit tests, helpers running nested substitutions); nothing
        // in this function prompts, so pausing cannot swallow a dialog.
        let _raw_pause = crate::repl::terminal_state::RawModePause::new();
        let (read_end, write_end) =
            crate::process::io::cloexec_pipe().context("failed to create substitution pipe")?;
        let (status_read, status_write) =
            crate::process::io::cloexec_pipe().context("failed to create status pipe")?;

        let snapshot = helper_snapshot(shell);
        // The substitution helper joins the shell's group so terminal
        // signals reach it together with the parent.
        let producer = spawn_plan_helper(
            &snapshot,
            plan,
            mode,
            PlanSignalPolicy::Normal,
            ChildStdio {
                stdin: parent_ctx.infile,
                stdout: write_end.as_raw_fd(),
                stderr: parent_ctx.errfile,
            },
            shell.pgid,
            Some(status_write.as_raw_fd()),
        )?;
        // The helper owns the write ends now (dup'd to its stdout/status
        // fd); closing ours lets the readers see EOF when the helper exits.
        drop(write_end);
        drop(status_write);

        let output = tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let mut file = std::fs::File::from(read_end);
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map(|_| buf)
        })
        .await
        .context("command substitution reader failed")?
        .context("failed to read command substitution output")?;

        // The one-byte verdict distinguishes denial from empty output: the
        // helper wrote it just before exiting, so it is always available by
        // now (EOF when the helper died first means "ran", not "denied").
        // Verdict first: a denial is authorization state, never an exit code,
        // so `D` must stay `AuthorizationCancelled` even though the helper
        // also exits 130.
        let mut verdict = [0u8; 1];
        let verdict_len = {
            use std::io::Read as _;
            let mut file = std::fs::File::from(status_read);
            file.read(&mut verdict).unwrap_or(0)
        };
        if verdict_len == 1 && verdict[0] == b'D' {
            // Reap before returning so the helper never lingers as a zombie.
            let _ = crate::process::wait_pid_job(producer, false);
            return Err(anyhow::anyhow!(AuthorizationCancelled));
        }
        if verdict_len == 1 && verdict[0] == b'E' {
            let _ = crate::process::wait_pid_job(producer, false);
            anyhow::bail!("isolated substitution helper failed");
        }

        // Reap the (already-exited) helper synchronously and read its real
        // shell exit code: a finite producer is gone by EOF, so this does
        // not block; signal deaths surface as 128+signal through the shared
        // wait mapping. Only `Completed` yields a status — `NoChild`
        // (ECHILD) is wait ownership, not an exit code, and every other
        // observation is an infrastructure error, never a synthesized status.
        let exit_code = match crate::process::wait_pid_job(producer, false) {
            Ok(WaitPidObservation::State(_, state @ ProcessState::Completed(_, _))) => state
                .shell_exit_code()
                .context("completed substitution helper has no exit code")?,
            Ok(WaitPidObservation::NoChild) => {
                anyhow::bail!("substitution helper status unavailable (ECHILD)")
            }
            Ok(WaitPidObservation::StillAlive) => {
                anyhow::bail!("substitution helper still alive after EOF")
            }
            Ok(WaitPidObservation::State(_, state)) => {
                anyhow::bail!("substitution helper in unexpected state: {state:?}")
            }
            Err(nix::errno::Errno::EINTR) => {
                // A signal interrupted the blocking wait; the helper already
                // closed its pipes, so one more wait observes its exit.
                match crate::process::wait_pid_job(producer, false) {
                    Ok(WaitPidObservation::State(_, state @ ProcessState::Completed(_, _))) => {
                        state
                            .shell_exit_code()
                            .context("completed substitution helper has no exit code")?
                    }
                    Ok(WaitPidObservation::NoChild) => {
                        anyhow::bail!("substitution helper status unavailable (ECHILD)")
                    }
                    Ok(other) => {
                        anyhow::bail!("substitution helper wait retry failed: {other:?}")
                    }
                    Err(retry_err) => {
                        anyhow::bail!("substitution helper wait failed: {retry_err}")
                    }
                }
            }
            Err(err) => {
                anyhow::bail!("substitution helper wait failed: {err}")
            }
        };

        Ok(CapturedSubstitution {
            stdout: String::from_utf8_lossy(&output).to_string(),
            exit_code,
        })
    })
}
