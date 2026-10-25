//! Per-stage pipeline launch: pipe wiring, automatic capture, ordered
//! redirect application, child spawn dispatch, and redirection-failure
//! unwind for [`JobProcess`].

use anyhow::{Context as _, Result};
use libc::STDERR_FILENO;
use nix::unistd::{Pid, getpid};
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use tracing::debug;

use super::builtin::BuiltinExecutionPlacement;
use super::builtin::builtin_execution_placement;
use super::fork::fork_process;
use super::io::{ConcurrentOutputDrain, OutputMonitor, cloexec_pipe, default_output_wiring};
use super::job_process::{JobProcess, ProcessLaunchOutcome, apply_pty_stdio};
use super::launch_outcome::CommandFailure;
use super::pty::PtyChildConfig;
use super::redirect::{self, Redirect};
use super::reexec::{spawn_background_builtin, spawn_isolated_builtin};
use crate::shell::Shell;
use dsh_types::Context;
use dsh_types::observed_output::ObservedStream;

impl JobProcess {
    pub(crate) async fn launch(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        stdout: RawFd,
        pty: Option<PtyChildConfig>,
        pipeline_context: bool,
    ) -> Result<ProcessLaunchOutcome> {
        let entry = (ctx.infile, ctx.outfile, ctx.errfile);
        let result = self
            .launch_stage(ctx, shell, stdout, pty, pipeline_context)
            .await;
        if result.is_err() {
            (ctx.infile, ctx.outfile, ctx.errfile) = entry;
        }
        result
    }

    async fn launch_stage(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        stdout: RawFd,
        pty: Option<PtyChildConfig>,
        pipeline_context: bool,
    ) -> Result<ProcessLaunchOutcome> {
        // has pipelines process ?
        let next_process = self.take_next();
        let has_next_process = next_process.is_some();
        // Apply the entire sequence after default pipe/PTY wiring: splitting
        // stdin from output changes both FD duplication and failure ordering.
        let redirects: Vec<Redirect> = self.redirects().to_vec();
        // Any redirection at all disables the automatic capture below, input
        // included: a command the user redirected should reach its destination
        // directly, without an automatic capture pipe.
        let has_redirect = !self.redirects().is_empty();
        let observe_foreground_external = ctx.output_observer.is_some()
            && ctx.foreground
            && matches!(self, JobProcess::Command(_))
            && !has_next_process
            && !has_redirect
            && pty.is_none()
            && ctx.captured_out.is_none();

        // Snapshot the wiring this call did not create. A redirection failure
        // below must close exactly the descriptors created here (pipeline
        // pipes, capture pipes, observer pipes) and put these slots back;
        // anything still naming an entry value is caller-owned and stays.
        //
        // NOTE: the entry `ctx.outfile` of a pipeline stage is *not* usable
        // as a "was this fd created here" probe: the previous stage leaves
        // its (already closed in the parent) pipe write end there until the
        // default wiring below replaces it. Fresh fds are tracked explicitly
        // instead — entry values only tell where to restore the slots to.
        let entry_infile = ctx.infile;
        let entry_outfile = ctx.outfile;
        let entry_errfile = ctx.errfile;
        // Write ends created by this call and now living in `ctx`, if any.
        let mut created_out: Option<OwnedFd> = None;
        let mut created_err: Option<OwnedFd> = None;
        // Capture monitors built before the spawn below. They stay local
        // until the child exists; only then does the outcome move them to
        // the job. Any earlier return drops them (closing the read ends)
        // with no child spawned, so no orphan path opens.
        let mut monitors: Vec<OutputMonitor> = Vec::new();

        let pipe_out = match next_process {
            Some(_) => {
                let (read, write) = cloexec_pipe().context("failed pipeline pipe")?;
                ctx.outfile = write.as_raw_fd();
                created_out = Some(write);
                Some(read)
            }
            None => {
                // Automatic capture for non-interactive mode (e.g. smart pipe tests)
                // We don't do this in interactive mode to preserve TTY (colors, etc.)
                // Async lists own their capture in `spawn_async_list`; the
                // generic pipe here would capture them twice.
                if (!ctx.interactive
                    && !has_redirect
                    && pty.is_none()
                    && ctx.captured_out.is_none()
                    && !matches!(self, JobProcess::AsyncList(_) | JobProcess::NoCommand(_)))
                    || observe_foreground_external
                {
                    let (read, write) = cloexec_pipe().context("failed pipe")?;
                    // The monitor is built before the write end leaves this
                    // scope: construction failure drops both ends via RAII
                    // with `ctx` untouched and no child spawned.
                    let monitor =
                        OutputMonitor::new_for_context(read, ctx, ObservedStream::Stdout)?;
                    ctx.outfile = write.as_raw_fd();
                    created_out = Some(write);
                    monitors.push(monitor);
                    None
                } else {
                    default_output_wiring(ctx, stdout);
                    None
                }
            }
        };

        if observe_foreground_external && ctx.errfile == STDERR_FILENO {
            let (read, write) = cloexec_pipe().context("failed stderr pipe")?;
            match OutputMonitor::new_for_context(read, ctx, ObservedStream::Stderr) {
                Ok(monitor) => {
                    ctx.errfile = write.as_raw_fd();
                    created_err = Some(write);
                    monitors.push(monitor);
                }
                Err(err) => {
                    // `read` closed inside the failed constructor, `write`
                    // drops here. The stdout capture wired above never
                    // reached a child: close its write end and restore the
                    // entry slots so nothing leaks and `ctx` names nothing
                    // closed.
                    if let Some(write_end) = created_out.take() {
                        drop(write_end);
                    }
                    ctx.outfile = entry_outfile;
                    ctx.errfile = entry_errfile;
                    return Err(err);
                }
            }
        }

        if let Some(pty) = pty {
            // PTY sets the default TTY fds. Output-only PTY keeps stdin on the
            // real terminal so normal shell typeahead remains available after
            // foreground commands finish.
            let slave_applied = apply_pty_stdio(ctx, pty.slave, pty.mode);

            debug!(
                "JOB_IO_SETUP: Job {} ({}) - final i/o: infile={}, outfile={}, errfile={} (slave={}, slave_applied={})",
                shell.get_job_id(),
                self.get_cmd(),
                ctx.infile,
                ctx.outfile,
                ctx.errfile,
                pty.slave,
                slave_applied
            );
        }

        // The write end the pipeline handed us, before any redirection had a
        // chance to replace it.
        let pipe_write = has_next_process.then_some(ctx.outfile);

        // A redirection failure is an ordinary command failure, not a runtime
        // error: unwind this call's pipe/capture wiring (the shell lives on,
        // so a leak here would be a persistent session leak) and report it
        // without the `?` operator.
        let applied = match redirect::apply(&redirects, ctx) {
            Ok(applied) => applied,
            Err(failure) => {
                self.abort_stage_wiring(
                    ctx,
                    (entry_infile, entry_outfile, entry_errfile),
                    pipe_out,
                    created_out,
                    created_err,
                );
                return Ok(ProcessLaunchOutcome::CommandFailed(
                    CommandFailure::redirect(&failure),
                ));
            }
        };

        self.set_io(ctx.infile, ctx.outfile, ctx.errfile);

        // initial pid
        let current_pid = getpid();

        let mut terminal_launch_failure = false;
        let launched: Result<Pid> = async {
            Ok(match self {
                JobProcess::Builtin(process) => {
                    match builtin_execution_placement(ctx.foreground, pipeline_context) {
                        BuiltinExecutionPlacement::Parent => {
                            process.pid = Some(current_pid);
                            // A sync builtin can fill its automatic capture pipe
                            // before returning. Temporarily give its reader an
                            // independent thread, retaining parent shell state.
                            let drain = monitors
                                .pop()
                                .map(ConcurrentOutputDrain::start)
                                .transpose()?;
                            let result = process.launch(ctx, shell).await;
                            if let Some(drain) = drain {
                                monitors.push(drain.finish()?);
                            }
                            result?;
                            current_pid
                        }
                        BuiltinExecutionPlacement::Reexec => {
                            // Pipeline members and background builtins share
                            // the isolated re-exec helper path.
                            let child_pid = if ctx.foreground && pipeline_context {
                                spawn_isolated_builtin(ctx, process, shell)?
                            } else {
                                spawn_background_builtin(ctx, process, shell)?
                            };
                            process.pid = Some(child_pid);
                            child_pid
                        }
                    }
                }
                JobProcess::Command(process) => {
                    // Bookkeeping for the current launch scope.
                    // `Job::launch` restores `ctx.process_count` to the
                    // caller's entry value on return.
                    ctx.process_count += 1;
                    // fork
                    // Explicit redirects stay authoritative. Only an unredirected
                    // FullProxy diagnostic may fall back from its revoked slave.
                    let diagnostic_fallback = pty
                        .filter(|pty| {
                            pty.mode == super::pty::PtyMode::FullProxy
                                && !has_redirect
                                && process.stderr == pty.slave
                        })
                        .map(|_| entry_errfile);
                    let (child, fork_monitors, reported_failure) = fork_process(
                        ctx,
                        ctx.pgid,
                        process,
                        shell,
                        pty,
                        diagnostic_fallback,
                        pipe_out.as_ref().map(AsRawFd::as_raw_fd),
                    )?;
                    terminal_launch_failure = reported_failure;
                    monitors.extend(fork_monitors);
                    child
                }
                JobProcess::SyntheticSource(process) => {
                    super::pipeline_source::spawn_synthetic_source(ctx, shell, process)?
                }
                JobProcess::NoCommand(process) => {
                    super::no_command_process::spawn_no_command_process(ctx, shell, process)?
                }
                JobProcess::AsyncList(_) => {
                    // Async lists never launch through the per-stage path:
                    // `Job::launch_process` routes them to
                    // `spawn_async_list`, which also owns the job-level
                    // pgid and monitor bookkeeping this path cannot do.
                    anyhow::bail!("async list cannot launch as a pipeline stage");
                }
            })
        }
        .await;

        // Restore before propagating: `applied` closes its files on drop, and
        // leaving `ctx` pointing at them would hand the next command a
        // descriptor that is already gone.
        let pid = match launched {
            Ok(pid) => pid,
            Err(err) => {
                // The child never spawned: `monitors` drops its readers
                // here. Close this call's capture write ends (the parent
                // copies the child never inherited) and the unread next-stage
                // pipe end before restoring the slots.
                if let Some(write_end) = created_out.take() {
                    drop(write_end);
                }
                if let Some(write_end) = created_err.take() {
                    drop(write_end);
                }
                if let Some(read_end) = pipe_out {
                    drop(read_end);
                }
                applied.restore(ctx);
                return Err(err);
            }
        };

        self.set_pid(Some(pid));

        // The process has the descriptors now (inherited at fork, or already
        // written to by an in-process builtin), so put `ctx` back before the
        // next command in the pipeline reads it.
        applied.restore(ctx);

        // `a > file | b` gives `a` the file instead of the pipe, which leaves
        // the shell holding the only remaining write end -- and `b` waiting
        // forever for an EOF that cannot arrive.
        if let Some(write_fd) = pipe_write {
            let (_, stdout_fd, stderr_fd) = self.get_io();
            if stdout_fd != write_fd && stderr_fd != write_fd {
                drop(created_out.take());
            }
        }

        // set pipe inout
        if let Some(pipe_out) = pipe_out {
            ctx.infile = pipe_out.into_raw_fd();
        }
        // return launched process pid, pipeline process, the descriptors
        // the redirections own (the caller must not close those itself),
        // and the pre-spawn capture monitors (moved once into the job by
        // the caller now that the child exists)
        // Successful spawn transfers parent-copy closure to Job. Until this
        // point every early return closes these writers through RAII.
        for writer in [created_out, created_err].into_iter().flatten() {
            let _ = writer.into_raw_fd();
        }
        Ok(ProcessLaunchOutcome::Launched {
            pid,
            terminal_launch_failure,
            next_process,
            redirects: applied,
            monitors,
        })
    }

    /// Close and unwind the pipe/capture wiring created by [`JobProcess::launch`]
    /// after a redirection failure, before anything spawned.
    ///
    /// Only descriptors created by that call are closed: `pipe_out` (the read
    /// end for the next stage) and `created_out` / `created_err` (fresh pipe
    /// write ends now living in `ctx`). Pre-spawn capture readers live in the
    /// local monitor vec and drop via RAII. Everything else in `ctx` is
    /// caller-owned (the caller's capture pipe, the base stdio) or job-owned
    /// (the PTY slave, only unwound, never closed), so the slots are simply
    /// restored to the entry values. No slot comparison is involved: a
    /// pipeline stage's entry `ctx.outfile` is the previous stage's
    /// already-closed pipe write end, which a fresh pipe could never be
    /// distinguished from by number alone.
    fn abort_stage_wiring(
        &mut self,
        ctx: &mut Context,
        entry: (RawFd, RawFd, RawFd),
        pipe_out: Option<OwnedFd>,
        created_out: Option<OwnedFd>,
        created_err: Option<OwnedFd>,
    ) {
        drop((pipe_out, created_out, created_err));
        // `redirect::apply` already rolled its own partial changes back, so
        // the slots now hold the post-wiring values; put back exactly what
        // the caller handed us.
        ctx.infile = entry.0;
        ctx.outfile = entry.1;
        ctx.errfile = entry.2;
    }
}
