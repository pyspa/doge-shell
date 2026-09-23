//! Foreground PTY setup, proxy tasks, and output capture.
//!
//! `setup_pty` commits transactionally: FullProxy needs output monitor plus
//! input proxy, OutputOnly needs output monitor, otherwise normal execution.
//! Every `launch_inner` error path reclaims proxy tasks via `cleanup_pty_tasks`.
//! Tests live in `job_pty/tests.rs` and never touch the real terminal.

use super::async_io::{AsyncPtyMasterWriter, AsyncStdin};
use super::job::Job;
use super::job_process::JobProcess;
use super::launch_outcome::JobLaunchOutcome;
use super::pty::{Pty, PtyChildConfig, PtyMode};
use super::state::ProcessState;
use crate::process::io::PtyMonitor;
use crate::process::job_wait::wait_job;
use crate::shell::Shell;
use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_types::Context;
use dsh_types::observed_output::SharedOutputObserver;
use libc::{STDIN_FILENO, STDOUT_FILENO};
use std::fs::File;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use tracing::{debug, error, warn};

const DOGESH_NO_PTY_ENV: &str = "DOGESH_NO_PTY";

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

    /// Raw-mode scope for an `fg`-resumed FullProxy interval.
    ///
    /// The initial launch uses [`Self::new`] (which also requires a
    /// foreground interactive `ctx`); a resume does not go through
    /// `launch_inner`, so this covers `resume input proxy → SIGCONT /
    /// foreground wait` with the same guard type. No new raw-mode
    /// mechanism: enablement still funnels through `enable_raw_mode` and
    /// restoration through `Drop`.
    ///
    /// The predicate is intentionally narrower than [`Self::new`]: this
    /// constructor is only called from `foreground_selected_job`, i.e. when
    /// a stopped FullProxy job is being brought into the foreground, so the
    /// foreground-interactive interval holds by construction. A bare
    /// `enable_raw_mode` failure (no TTY, tests) degrades to disabled
    /// without touching terminal state.
    pub(crate) fn for_resume(job: &Job) -> Self {
        if uses_full_pty_proxy(job) && !is_builtin_job(job) {
            match enable_raw_mode() {
                Ok(()) => Self { enabled: true },
                Err(err) => {
                    debug!("fg resume raw mode not enabled: {}", err);
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

/// Decide whether an interactive foreground external command should run behind a
/// full PTY proxy (stdin/stdout/stderr all on the PTY, real terminal switched to
/// raw mode, input proxied to the master) instead of the output-only proxy.
///
/// A coherent terminal is required for curses/TUI programs (e.g. `tig`, `less`,
/// `vim`): they configure raw mode on their output fd and read keystrokes from
/// the same terminal, so splitting input (real terminal) and output (PTY slave)
/// leaves the input side in cooked mode and breaks key bindings.
///
/// We only use the full proxy when both the command's input and output go to the
/// terminal (neither redirected nor piped to/from another command). Otherwise the
/// child's stdin is never moved onto the PTY by `apply_pty_stdio`, so selecting
/// FullProxy would leave the input proxy running for a program that reads from a
/// file/pipe instead, and piped/redirected commands are not interactive TUIs.
fn should_use_full_proxy(job: &Job, ctx: &Context) -> bool {
    ctx.foreground
        && ctx.interactive
        && !is_builtin_job(job)
        && ctx.infile == STDIN_FILENO
        && ctx.outfile == STDOUT_FILENO
        && !job_has_redirects(job)
}

/// Whether the command carries redirections of its own.
///
/// The `ctx` checks above cannot see these: redirections are applied inside
/// `launch`, after this decision has been made. Without this, `vim > out` would
/// still take the terminal into raw mode and proxy keystrokes for a command
/// whose output never reaches the terminal.
fn job_has_redirects(job: &Job) -> bool {
    let mut current = job.process.as_deref();
    while let Some(process) = current {
        if !process.redirects().is_empty() {
            return true;
        }
        current = process.next_process();
    }
    false
}

fn should_enable_foreground_pty_raw_mode(job: &Job, ctx: &Context) -> bool {
    ctx.foreground
        && ctx.interactive
        && job.pty.is_some()
        && job.pty_mode == Some(PtyMode::FullProxy)
        && !is_builtin_job(job)
}

pub(crate) fn uses_full_pty_proxy(job: &Job) -> bool {
    job.pty.is_some() && job.pty_mode == Some(PtyMode::FullProxy)
}

/// PTY setup transaction invariant.
///
/// `setup_pty()` returns only one of three committed states:
///
/// A. FullProxy: `job.pty` + `job.pty_mode == FullProxy` + output task +
///    input task, and the returned [`PtyChildConfig`]`mode` is FullProxy.
/// B. OutputOnly: `job.pty` + `job.pty_mode == OutputOnly` + output task and
///    no input task, and the returned config mode is OutputOnly.
/// C. Normal execution: no PTY state on the job and `Ok(None)` returned.
///
/// Forbidden states: FullProxy without an input task, any PTY without an
/// output monitor, a returned child mode that differs from `job.pty_mode`,
/// or leftover proxy tasks after returning `None`.
///
/// PTY state is committed to `Job` only after required output monitoring
/// has been prepared. A committed FullProxy always has both output monitoring
/// and an input proxy. Failure to prepare input downgrades to OutputOnly;
/// failure to prepare output monitoring discards the PTY entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtyMasterUse {
    Output,
    Input,
}

/// Deterministic failure-injection seam for PTY setup.
///
/// Production uses [`SystemPtySetupOps`]; tests implement this trait with a
/// fault-injecting struct. The seam covers the five fallible PTY
/// infrastructure steps (`Pty::new`, output master clone, `PtyMonitor::new`,
/// input master clone, `AsyncPtyMasterWriter::new`) without touching fd
/// limits, timing, or the real terminal.
pub(crate) trait PtySetupOps {
    fn new_pty(&self) -> Result<Pty>;
    fn clone_master(&self, pty: &Pty, purpose: PtyMasterUse) -> Result<File>;
    fn new_monitor(
        &self,
        master: File,
        observer: Option<SharedOutputObserver>,
    ) -> Result<PtyMonitor>;
    fn new_writer(&self, master: File) -> std::io::Result<AsyncPtyMasterWriter>;
}

struct SystemPtySetupOps;

impl PtySetupOps for SystemPtySetupOps {
    fn new_pty(&self) -> Result<Pty> {
        Pty::new()
    }

    fn clone_master(&self, pty: &Pty, _purpose: PtyMasterUse) -> Result<File> {
        pty.try_clone_master()
    }

    fn new_monitor(
        &self,
        master: File,
        observer: Option<SharedOutputObserver>,
    ) -> Result<PtyMonitor> {
        let master_fd = master.into_raw_fd();
        PtyMonitor::new(master_fd, observer)
    }

    fn new_writer(&self, master: File) -> std::io::Result<AsyncPtyMasterWriter> {
        prepare_pty_input_writer(master)
    }
}

/// Synchronous construction of the PTY input writer.
///
/// This is the fallible sync half of the input proxy: failure here must keep
/// the caller from committing FullProxy (downgrade to OutputOnly instead).
/// Task spawning happens separately in [`spawn_pty_input_proxy_with`].
fn prepare_pty_input_writer(master: File) -> std::io::Result<AsyncPtyMasterWriter> {
    AsyncPtyMasterWriter::new(master)
}

/// Spawn the input proxy task for an already-constructed writer.
///
/// `open_input` runs inside the task so a slow `/dev/tty` open never blocks
/// PTY setup; see [`setup_pty_with`] for why it is injectable.
fn spawn_pty_input_proxy_with<F>(
    mut writer: AsyncPtyMasterWriter,
    open_input: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnOnce() -> std::io::Result<AsyncStdin> + Send + 'static,
{
    tokio::spawn(async move {
        match open_input() {
            Ok(mut async_stdin) => {
                if let Err(err) = tokio::io::copy(&mut async_stdin, &mut writer).await {
                    debug!("PTY input proxy stopped: {}", err);
                }
            }
            // Falling back to stdin means reading the real terminal, so
            // it is off-limits when we do not own it (tests).
            Err(err) if crate::terminal::terminal_control_enabled() => {
                warn!(
                    "Failed to open an independent /dev/tty input handle, falling back to Tokio stdin: {}",
                    err
                );
                let mut std_stdin = tokio::io::stdin();
                if let Err(err) = tokio::io::copy(&mut std_stdin, &mut writer).await {
                    debug!("Fallback PTY input proxy stopped: {}", err);
                }
            }
            Err(err) => {
                debug!("PTY input proxy not started: {}", err);
            }
        }
    })
}

pub(crate) async fn setup_pty(job: &mut Job, ctx: &mut Context) -> Result<Option<PtyChildConfig>> {
    setup_pty_with(job, ctx, AsyncStdin::open_tty).await
}

/// `open_input` is injectable so tests can point the proxy at a PTY of their
/// own instead of the real controlling terminal (which would swallow the
/// developer's keystrokes under `cargo test`).
pub(crate) async fn setup_pty_with<F>(
    job: &mut Job,
    ctx: &mut Context,
    open_input: F,
) -> Result<Option<PtyChildConfig>>
where
    F: FnOnce() -> std::io::Result<AsyncStdin> + Send + 'static,
{
    let ops = SystemPtySetupOps;
    setup_pty_with_ops(job, ctx, open_input, &ops).await
}

/// Transactional PTY setup: prepare everything with locals, then commit once.
///
/// No `job.pty*` field is touched until output monitoring (mandatory) and,
/// for FullProxy, the input writer (downgradable) are both resolved. The
/// effective mode decided here is the single source of truth for both the
/// committed `job.pty_mode` and the returned [`PtyChildConfig`], so the child
/// stdio wiring and the parent process-group path can never see a stale mode.
async fn setup_pty_with_ops<Ops, F>(
    job: &mut Job,
    ctx: &mut Context,
    open_input: F,
    ops: &Ops,
) -> Result<Option<PtyChildConfig>>
where
    Ops: PtySetupOps,
    F: FnOnce() -> std::io::Result<AsyncStdin> + Send + 'static,
{
    if !should_create_pty(
        ctx,
        job.disable_pty,
        std::env::var(DOGESH_NO_PTY_ENV).is_ok(),
    ) {
        return Ok(None);
    }

    // PTY itself is an optional optimization: failure falls back to normal
    // execution instead of failing the user command.
    let pty = match ops.new_pty() {
        Ok(pty) => {
            debug!("PTY created: {:?}", pty);
            pty
        }
        Err(e) => {
            error!(
                "Failed to create PTY: {}, falling back to normal execution",
                e
            );
            return Ok(None);
        }
    };

    let intended_mode = if should_use_full_proxy(job, ctx) {
        PtyMode::FullProxy
    } else {
        PtyMode::OutputOnly
    };
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let _ = pty.resize(rows, cols);
    }

    // Output monitoring is mandatory: a PTY without a master reader would
    // hide output or block the child once the PTY buffer fills, so any
    // failure here discards the PTY entirely.
    let output_master = match ops.clone_master(&pty, PtyMasterUse::Output) {
        Ok(master) => master,
        Err(e) => {
            error!(
                "Failed to clone PTY for output: {}, falling back to normal execution",
                e
            );
            drop(pty);
            return Ok(None);
        }
    };
    let mut monitor = match ops.new_monitor(output_master, ctx.output_observer.clone()) {
        Ok(monitor) => monitor,
        Err(e) => {
            error!(
                "Failed to create PTY output monitor: {}, falling back to normal execution",
                e
            );
            drop(pty);
            return Ok(None);
        }
    };

    // Input is best-effort: when only the input side fails, output
    // monitoring is still useful, so downgrade to OutputOnly instead of
    // discarding the PTY.
    let mut effective_mode = intended_mode;
    let mut prepared_writer = None;
    if intended_mode == PtyMode::FullProxy {
        match ops.clone_master(&pty, PtyMasterUse::Input) {
            Ok(input_master) => match ops.new_writer(input_master) {
                Ok(writer) => {
                    prepared_writer = Some(writer);
                }
                Err(e) => {
                    error!(
                        "Failed to create PTY input writer, falling back to output-only: {}",
                        e
                    );
                    effective_mode = PtyMode::OutputOnly;
                }
            },
            Err(e) => {
                // A clone failure here is transient (e.g. fd exhaustion).
                // Fall back to output-only so the command still runs.
                error!(
                    "Failed to clone PTY for input proxy, falling back to output-only: {}",
                    e
                );
                effective_mode = PtyMode::OutputOnly;
            }
        }
    }

    // All fallible preparation is done: spawn tasks, then commit at once.
    let output_task = tokio::spawn(async move {
        monitor.process_output().await?;
        Ok(String::from_utf8_lossy(&monitor.captured_output).to_string())
    });
    let input_task = match (effective_mode, prepared_writer) {
        (PtyMode::FullProxy, Some(writer)) => Some(spawn_pty_input_proxy_with(writer, open_input)),
        // Defensive: FullProxy without a writer must never commit (forbidden
        // state). Unreachable today because every writer failure already
        // downgrades `effective_mode`, but an explicit arm keeps a future
        // edit from silently reintroducing FullProxy + no input task.
        (PtyMode::FullProxy, None) => {
            error!("PTY input writer missing for FullProxy, falling back to output-only");
            effective_mode = PtyMode::OutputOnly;
            None
        }
        // Downgraded or output-only: drop the unused input opener without
        // calling it so tests (and non-terminal runs) never touch a terminal.
        _ => None,
    };

    let slave_fd = pty.slave.as_raw_fd();
    let child_config = PtyChildConfig {
        slave: slave_fd,
        mode: effective_mode,
    };
    job.pty = Some(pty);
    job.pty_mode = Some(effective_mode);
    job.pty_output_task = Some(output_task);
    job.pty_input_task = input_task;
    Ok(Some(child_config))
}

async fn suspend_pty_input_proxy(job: &mut Job) {
    if let Some(input_task) = job.pty_input_task.take() {
        input_task.abort();
        let _ = input_task.await;
    }
}

/// Suspend foreground terminal-input ownership for a stopped job.
///
/// Keeps `job.pty`, `job.pty_mode`, and `job.pty_output_task` (PTY session
/// and captured output stay owned by the stopped job); only the input
/// proxy is stopped so the shell prompt never shares `/dev/tty` reads with
/// a stopped FullProxy job. Never awaits output: a stopped child still
/// holds the PTY slave, so EOF may never arrive.
pub(crate) async fn suspend_stopped_pty_input(job: &mut Job) {
    suspend_pty_input_proxy(job).await;
}

pub async fn cleanup_pty_tasks(job: &mut Job) {
    suspend_pty_input_proxy(job).await;
    if let Some(output_task) = job.pty_output_task.take() {
        output_task.abort();
        let _ = output_task.await;
    }
    job.pty = None;
    job.pty_mode = None;
}

/// Recreate exactly one input proxy for a stopped FullProxy job being
/// resumed into the foreground.
///
/// Transactional: master clone / writer construction happen before any
/// `job` mutation, so preparation failure leaves `pty` / `pty_output_task`
/// / `pty_mode` untouched with `pty_input_task == None`, and the caller
/// can requeue the job as `Stopped` before any SIGCONT. Non-FullProxy
/// jobs are a no-op; an existing input task is never duplicated.
pub(crate) async fn resume_pty_input_proxy(job: &mut Job) -> Result<()> {
    resume_pty_input_proxy_with(job, AsyncStdin::open_tty).await
}

pub(crate) async fn resume_pty_input_proxy_with<F>(job: &mut Job, open_input: F) -> Result<()>
where
    F: FnOnce() -> std::io::Result<AsyncStdin> + Send + 'static,
{
    let ops = SystemPtySetupOps;
    resume_pty_input_proxy_with_ops(job, open_input, &ops).await
}

pub(crate) async fn resume_pty_input_proxy_with_ops<Ops, F>(
    job: &mut Job,
    open_input: F,
    ops: &Ops,
) -> Result<()>
where
    Ops: PtySetupOps,
    F: FnOnce() -> std::io::Result<AsyncStdin> + Send + 'static,
{
    // Note: `async` for call-site uniformity with the other PTY helpers
    // (`resume_pty_input_proxy`, `suspend_stopped_pty_input`); the body
    // itself performs no `.await` — preparation is synchronous and the
    // spawned proxy task runs detached.
    if job.pty_mode != Some(PtyMode::FullProxy) {
        return Ok(());
    }
    if job.pty_input_task.is_some() {
        return Ok(());
    }
    let Some(pty) = job.pty.as_ref() else {
        anyhow::bail!(
            "cannot resume FullProxy input for job {} without a PTY",
            job.job_id
        );
    };
    if job.pty_output_task.is_none() {
        anyhow::bail!(
            "cannot resume FullProxy input for job {} without output ownership",
            job.job_id
        );
    }
    let input_master = ops.clone_master(pty, PtyMasterUse::Input)?;
    let writer = ops
        .new_writer(input_master)
        .map_err(|err| anyhow::anyhow!("failed to prepare resumed PTY input writer: {err}"))?;
    job.pty_input_task = Some(spawn_pty_input_proxy_with(writer, open_input));
    Ok(())
}

pub async fn manage_execution(job: &mut Job, ctx: &mut Context) -> Result<()> {
    // An asynchronous launch never waits here — not even in
    // non-interactive (`-c`) runs. The next AND-OR list must start without
    // waiting for background completion; command mode releases known-async
    // ownership once at its normal-exit boundary instead (see
    // `Shell::detach_known_async_jobs_for_normal_exit`). Foreground jobs
    // still wait synchronously in every mode.
    if ctx.foreground {
        if ctx.interactive {
            if ctx.process_count > 0 {
                crate::process::job_wait::put_in_foreground(job, false, false).await?;
            }
        } else {
            debug!(
                "JOB_LAUNCH_NON_INTERACTIVE: Non-interactive mode, waiting for job {} completion",
                job.job_id
            );
            wait_job(job, false).await?;
        }
    } else {
        crate::process::job_wait::put_in_background(job).await?;
    }
    Ok(())
}

/// Canonical foreground launch settlement shared by `Job::launch_inner`.
///
/// The outcome is always derived from the canonical tree via
/// `refresh_lifecycle_state`, never tail-only. Completion-only I/O
/// finalization runs solely for completed trees; stopped trees keep
/// PTY/output with only the input proxy suspended.
pub(crate) async fn settle_foreground_launch(
    job: &mut Job,
    ctx: &Context,
    shell: &mut Shell,
) -> Result<JobLaunchOutcome> {
    job.refresh_lifecycle_state();
    if !job.foreground {
        debug!(
            "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
            job.job_id, job.state, job.foreground
        );
        return Ok(JobLaunchOutcome::Process(ProcessState::Running));
    }
    match job.state {
        ProcessState::Completed(_, _) => {
            if let Err(err) = capture_completed_output_and_history(job, ctx, shell).await {
                cleanup_pty_tasks(job).await;
                return Err(err);
            }
            debug!(
                "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
                job.job_id, job.state, job.foreground
            );
            Ok(JobLaunchOutcome::Process(job.state))
        }
        ProcessState::Stopped(_, _) => {
            suspend_stopped_pty_input(job).await;
            debug!(
                "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
                job.job_id, job.state, job.foreground
            );
            Ok(JobLaunchOutcome::Process(job.state))
        }
        ProcessState::Running => {
            if job.has_process() {
                cleanup_pty_tasks(job).await;
                anyhow::bail!(
                    "foreground wait returned with active job {} ('{}', state: {:?})",
                    job.job_id,
                    job.cmd,
                    job.state,
                );
            }
            debug!(
                "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
                job.job_id, job.state, job.foreground
            );
            Ok(JobLaunchOutcome::Process(job.state))
        }
    }
}

pub async fn capture_completed_output_and_history(
    job: &mut Job,
    ctx: &Context,
    shell: &mut Shell,
) -> Result<()> {
    // Completion-only finalization: a stopped or otherwise incomplete
    // process-bearing tree must never reach output await / history here.
    // Callers settle stopped jobs via `suspend_stopped_pty_input` instead.
    if job.has_process() && !job.is_process_tree_completed() {
        anyhow::bail!(
            "cannot finalize output for incomplete job {} ('{}', state: {:?})",
            job.job_id,
            job.cmd,
            job.state,
        );
    }
    let mut stdout_cap = String::new();
    let mut stderr_cap = String::new();

    suspend_pty_input_proxy(job).await;
    job.pty = None;
    job.pty_mode = None;

    if let Some(output_task) = job.pty_output_task.take() {
        match output_task.await {
            Ok(Ok(output)) => stdout_cap = output,
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(e.into()),
        }
    } else {
        // History captures the pipeline tail's output, not the head's: only
        // the tail ever owns capture monitors (a head stage feeds the pipe,
        // never a capture pipe), so the first monitor per stream is the
        // tail's. Selecting by stream keeps this true without raw-fd
        // bookkeeping on the process tree.
        use dsh_types::observed_output::ObservedStream;
        if let Some(monitor) = job
            .monitors
            .iter()
            .find(|monitor| monitor.stream() == ObservedStream::Stdout)
        {
            stdout_cap = monitor.captured_output.clone();
        }
        if let Some(monitor) = job
            .monitors
            .iter()
            .find(|monitor| monitor.stream() == ObservedStream::Stderr)
        {
            stderr_cap = monitor.captured_output.clone();
        }
    }

    if (!stdout_cap.is_empty() || !stderr_cap.is_empty()) && ctx.foreground {
        use dsh_types::output_history::OutputEntry;
        let stdout_stripped = console::strip_ansi_codes(&stdout_cap).to_string();
        let stderr_stripped = console::strip_ansi_codes(&stderr_cap).to_string();

        // A completed process-bearing tree always has a logical status;
        // inventing `0` here would record synthetic success for stopped /
        // incomplete jobs. Process-less jobs keep the historical no-process
        // semantics.
        let exit_code = if job.has_process() {
            job.final_exit_status().ok_or_else(|| {
                anyhow::anyhow!(
                    "completed job {} ('{}') has no final status",
                    job.job_id,
                    job.cmd
                )
            })?
        } else {
            job.final_exit_status().unwrap_or(0)
        };

        let entry = OutputEntry::new(job.cmd.clone(), stdout_stripped, stderr_stripped, exit_code);
        shell
            .environment
            .write()
            .session_output_state
            .output_history
            .push(entry);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
