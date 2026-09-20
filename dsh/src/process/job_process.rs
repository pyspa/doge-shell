use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::Signal;
use nix::unistd::{Pid, close, getpid};
use std::os::fd::IntoRawFd;
use std::os::unix::io::RawFd;
use tracing::debug;

use super::builtin::BuiltinExecutionPlacement;
use super::builtin::{BuiltinProcess, builtin_execution_placement};
use super::fork::fork_process;
use super::io::{cloexec_pipe, create_pipe, default_output_wiring};
use super::launch_outcome::CommandFailure;
use super::pipeline_source::PipelineSourceProcess;
use super::process::Process;
use super::pty::{PtyChildConfig, PtyMode};
use super::redirect::{self, AppliedRedirects, Redirect};
use super::reexec::{spawn_background_builtin, spawn_isolated_builtin};
use super::state::ProcessState;
use crate::shell::Shell;
use dsh_types::Context;

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Command(Process),
    SyntheticSource(PipelineSourceProcess),
}

impl std::fmt::Debug for JobProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        match self {
            JobProcess::Builtin(jprocess) => f
                .debug_struct("JobProcess::Builtin")
                .field("cmd", &jprocess.argv)
                .field("has_next", &jprocess.next.is_some())
                .finish(),
            JobProcess::Command(jprocess) => f
                .debug_struct("JobProcess::Command")
                .field("cmd", &jprocess.cmd)
                .field("argv", &jprocess.argv)
                .field("pid", &jprocess.pid)
                .field("stdin", &jprocess.stdin)
                .field("stdout", &jprocess.stdout)
                .field("stderr", &jprocess.stderr)
                .field("has_next", &jprocess.next.is_some())
                .field("state", &jprocess.state)
                .finish(),
            JobProcess::SyntheticSource(jprocess) => f
                .debug_struct("JobProcess::SyntheticSource")
                .field("data_len", &jprocess.data.len())
                .field("has_next", &jprocess.next.is_some())
                .finish(),
        }
    }
}

pub(crate) fn apply_pty_stdio(ctx: &mut Context, slave: RawFd, pty_mode: PtyMode) -> bool {
    let mut slave_applied = false;
    if pty_mode == PtyMode::FullProxy && ctx.infile == STDIN_FILENO {
        ctx.infile = slave;
        slave_applied = true;
    }
    if ctx.outfile == STDOUT_FILENO {
        ctx.outfile = slave;
        slave_applied = true;
    }
    if ctx.errfile == STDERR_FILENO {
        ctx.errfile = slave;
        slave_applied = true;
    }
    slave_applied
}

/// `argv` as stored includes the program at index 0; safety classification
/// takes the program separately, so hand back only the arguments.
fn strip_argv0<'a>(program: &str, argv: &'a [String]) -> &'a [String] {
    match argv.first() {
        Some(first) if first == program => &argv[1..],
        _ => argv,
    }
}

/// What one `JobProcess::launch` reports.
///
/// `Launched` carries the child pid, the detached downstream stages, and the
/// redirection guard (the caller owns its files and must not close those
/// descriptors itself). `CommandFailed` is a redirection setup failure: the
/// stage never spawned, its temporary pipe/capture wiring is already closed
/// and unwound, and the evaluator reports it as an ordinary command failure.
/// Only internal failures (pipe creation, spawn protocol) travel as `Err`.
#[derive(Debug)]
pub(crate) enum ProcessLaunchOutcome {
    Launched {
        pid: Pid,
        next_process: Option<Box<JobProcess>>,
        redirects: AppliedRedirects,
    },
    CommandFailed(CommandFailure),
}

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
            JobProcess::SyntheticSource(jprocess) => jprocess.link(process),
        }
    }

    /// The next stage of the pipeline, borrowed rather than cloned.
    pub(crate) fn next_process(&self) -> Option<&JobProcess> {
        match self {
            JobProcess::Builtin(p) => p.next.as_deref(),
            JobProcess::Command(p) => p.next.as_deref(),
            JobProcess::SyntheticSource(p) => p.next.as_deref(),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::SyntheticSource(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn mut_next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::SyntheticSource(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn take_next(&mut self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.take(),
            JobProcess::Command(jprocess) => jprocess.next.take(),
            JobProcess::SyntheticSource(jprocess) => jprocess.next.take(),
        }
    }

    pub fn set_io(&mut self, stdin: RawFd, stdout: RawFd, stderr: RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::Command(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::SyntheticSource(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
        }
    }

    pub fn get_io(&self) -> (RawFd, RawFd, RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::SyntheticSource(jprocess) => {
                (jprocess.stdin, jprocess.stdout, jprocess.stderr)
            }
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(process) => {
                process.pid = pid;
            }
            JobProcess::Command(process) => {
                process.pid = pid;
            }
            JobProcess::SyntheticSource(process) => {
                process.pid = pid;
            }
        }
    }

    pub fn get_pid(&self) -> Option<Pid> {
        match self {
            JobProcess::Builtin(process) => process.pid,
            JobProcess::Command(process) => process.pid,
            JobProcess::SyntheticSource(process) => process.pid,
        }
    }

    /// The child pid this node owns, if any.
    ///
    /// A foreground in-process builtin carries `pid == shell_pid`, which is
    /// the shell itself — never a waitable/killable child. Only a pid owned
    /// by the parent shell (background re-exec helper or external command)
    /// is lifecycle-managed.
    pub(crate) fn owned_child_pid(&self, shell_pid: Pid) -> Option<Pid> {
        self.get_pid().filter(|pid| *pid != shell_pid)
    }

    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
            JobProcess::SyntheticSource(p) => p.state = state,
        }
    }

    pub(crate) fn set_state_pid(&mut self, pid: Pid, state: ProcessState) -> bool {
        debug!(
            "🔄 STATE: set_state_pid called for pid: {}, state: {:?}",
            pid, state
        );
        let result = match self {
            JobProcess::Builtin(p) => {
                debug!("🔄 STATE: Setting state for builtin process: {}", p.name);
                p.set_state(pid, state)
            }
            JobProcess::Command(p) => {
                debug!("🔄 STATE: Setting state for command process: {}", p.cmd);
                p.set_state(pid, state)
            }
            JobProcess::SyntheticSource(p) => {
                debug!("🔄 STATE: Setting state for pipeline source");
                p.set_state(pid, state)
            }
        };
        debug!("🔄 STATE: set_state_pid result: {}", result);
        result
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
            JobProcess::Command(p) => p.state,
            JobProcess::SyntheticSource(p) => p.state,
        }
    }

    /// Whether the whole job can be treated as stopped: at least one
    /// `Stopped` stage exists and no `Running` stage remains.
    ///
    /// `Completed` stages are neutral (already out of the lifecycle), so
    /// `Completed / Stopped` is fully stopped while `Running / Stopped`
    /// (partial stop) is not. All-completed pipelines are *not* stopped.
    pub(crate) fn is_fully_stopped(&self) -> bool {
        let mut saw_stopped = false;
        let mut current = Some(self);
        while let Some(process) = current {
            match process.get_state() {
                ProcessState::Running => return false,
                ProcessState::Stopped(_, _) => {
                    saw_stopped = true;
                }
                ProcessState::Completed(_, _) => {}
            }
            current = process.next_process();
        }
        saw_stopped
    }

    pub(crate) fn is_completed(&self) -> bool {
        let mut current = Some(self);
        while let Some(process) = current {
            if !matches!(process.get_state(), ProcessState::Completed(_, _)) {
                return false;
            }
            current = process.next_process();
        }
        true
    }

    /// Check if any process in the pipeline is stopped
    pub(crate) fn has_stopped_process(&self) -> bool {
        let mut current = Some(self);
        while let Some(process) = current {
            if matches!(process.get_state(), ProcessState::Stopped(_, _)) {
                return true;
            }
            current = process.next_process();
        }
        false
    }

    /// Mark every stopped pipeline stage as running after a successful resume.
    ///
    /// Completed stages stay completed, and traversal borrows the actual tree
    /// mutably so the transition is not lost in one of the clone-based accessors.
    pub(crate) fn mark_stopped_processes_running(&mut self) {
        match self {
            JobProcess::Builtin(process) => {
                if matches!(process.state, ProcessState::Stopped(_, _)) {
                    process.state = ProcessState::Running;
                }
                if let Some(next) = process.next.as_deref_mut() {
                    next.mark_stopped_processes_running();
                }
            }
            JobProcess::Command(process) => {
                if matches!(process.state, ProcessState::Stopped(_, _)) {
                    process.state = ProcessState::Running;
                }
                if let Some(next) = process.next.as_deref_mut() {
                    next.mark_stopped_processes_running();
                }
            }
            JobProcess::SyntheticSource(process) => {
                if matches!(process.state, ProcessState::Stopped(_, _)) {
                    process.state = ProcessState::Running;
                }
                if let Some(next) = process.next.as_deref_mut() {
                    next.mark_stopped_processes_running();
                }
            }
        }
    }

    /// First `Stopped` state in pipeline order, if any.
    ///
    /// Used to sync the job-table summary state after a foreground wait:
    /// the actual `waitpid`-observed `(pid, signal)` must be reused, never
    /// synthesized.
    pub(crate) fn first_stopped_state(&self) -> Option<ProcessState> {
        let mut current = Some(self);
        while let Some(process) = current {
            if let stopped @ ProcessState::Stopped(_, _) = process.get_state() {
                return Some(stopped);
            }
            current = process.next_process();
        }
        None
    }

    pub fn get_cap_out(&self) -> (Option<RawFd>, Option<RawFd>) {
        match self {
            JobProcess::Builtin(p) => (p.cap_stdout, p.cap_stderr),
            JobProcess::Command(p) => (p.cap_stdout, p.cap_stderr),
            JobProcess::SyntheticSource(_) => (None, None),
        }
    }

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
            JobProcess::Command(p) => &p.cmd,
            JobProcess::SyntheticSource(_) => "<smart-pipe-source>",
        }
    }

    /// Concrete program plus argv for safety classification of materialized
    /// jobs. Read-only: the guard must see the post-substitution argv, not
    /// just the raw source line.
    ///
    /// The slice excludes `argv[0]` (the program itself): `classify_tokens`
    /// takes the program separately and would otherwise see it twice, which
    /// broke `git` subcommand and interpreter-flag detection for dynamic
    /// commands (`$(printf git) push --force`).
    ///
    /// A synthetic source is not a command: it yields `None` and the guard
    /// skips it.
    pub(crate) fn command_argv(&self) -> Option<(&str, &[String])> {
        match self {
            JobProcess::Builtin(p) => Some((p.name.as_str(), strip_argv0(&p.name, &p.argv))),
            JobProcess::Command(p) => Some((p.cmd.as_str(), strip_argv0(&p.cmd, &p.argv))),
            JobProcess::SyntheticSource(_) => None,
        }
    }

    /// Redirections written on this command.
    pub(crate) fn redirects(&self) -> &[Redirect] {
        static EMPTY_REDIRECTS: &[Redirect] = &[];
        match self {
            JobProcess::Builtin(p) => &p.redirects,
            JobProcess::Command(p) => &p.redirects,
            JobProcess::SyntheticSource(_) => EMPTY_REDIRECTS,
        }
    }

    pub(crate) fn set_redirects(&mut self, redirects: Vec<Redirect>) {
        match self {
            JobProcess::Builtin(p) => p.redirects = redirects,
            JobProcess::Command(p) => p.redirects = redirects,
            JobProcess::SyntheticSource(_) => {
                super::pipeline_source::assert_no_source_redirects(redirects.len());
            }
        }
    }

    pub(crate) fn set_env_overrides(&mut self, overrides: Vec<(String, String)>) {
        match self {
            JobProcess::Builtin(p) => p.env_overrides = overrides,
            JobProcess::Command(p) => p.env_overrides = overrides,
            JobProcess::SyntheticSource(_) => {
                super::pipeline_source::assert_no_source_env(overrides.len());
            }
        }
    }

    pub(crate) async fn launch(
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
        let output_redirects: Vec<Redirect> = self
            .redirects()
            .iter()
            .filter(|redirect| !redirect.is_stdin())
            .cloned()
            .collect();
        // Any redirection at all disables the automatic capture below, input
        // included: capture reroutes stdout through a monitor that reformats
        // line endings, and a command the user redirected should reach its
        // destination byte for byte.
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
        let mut created_out: Option<RawFd> = None;
        let mut created_err: Option<RawFd> = None;

        let pipe_out = match next_process {
            Some(_) => {
                let pipe = create_pipe(ctx)?; // create pipe
                created_out = Some(ctx.outfile);
                pipe
            }
            None => {
                // Automatic capture for non-interactive mode (e.g. smart pipe tests)
                // We don't do this in interactive mode to preserve TTY (colors, etc.)
                if (!ctx.interactive
                    && !has_redirect
                    && pty.is_none()
                    && ctx.captured_out.is_none())
                    || observe_foreground_external
                {
                    let (pout, pin) = cloexec_pipe().context("failed pipe")?;
                    ctx.outfile = pin.into_raw_fd();
                    created_out = Some(ctx.outfile);
                    let pout_raw = pout.into_raw_fd();
                    match self {
                        JobProcess::Builtin(p) => p.cap_stdout = Some(pout_raw),
                        JobProcess::Command(p) => p.cap_stdout = Some(pout_raw),
                        JobProcess::SyntheticSource(_) => {}
                    }
                    None
                } else {
                    default_output_wiring(ctx, stdout);
                    None
                }
            }
        };

        if observe_foreground_external && ctx.errfile == STDERR_FILENO {
            let (pout, pin) = cloexec_pipe().context("failed stderr pipe")?;
            ctx.errfile = pin.into_raw_fd();
            created_err = Some(ctx.errfile);
            let pout_raw = pout.into_raw_fd();
            if let JobProcess::Command(p) = self {
                p.cap_stderr = Some(pout_raw);
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
        let applied = match redirect::apply(&output_redirects, ctx) {
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

        let launched: Result<Pid> = async {
            Ok(match self {
                JobProcess::Builtin(process) => {
                    match builtin_execution_placement(ctx.foreground, pipeline_context) {
                        BuiltinExecutionPlacement::Parent => {
                            process.pid = Some(current_pid);
                            process.launch(ctx, shell).await?;
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
                    ctx.process_count += 1;
                    // fork
                    fork_process(ctx, ctx.pgid, process, shell, pty)?
                }
                JobProcess::SyntheticSource(process) => {
                    super::pipeline_source::spawn_synthetic_source(ctx, shell, process)?
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
            if stdout_fd != write_fd
                && stderr_fd != write_fd
                && let Err(err) = close(write_fd)
            {
                debug!("failed to close superseded pipe write end: {}", err);
            }
        }

        // set pipe inout
        if let Some(pipe_out) = pipe_out {
            ctx.infile = pipe_out;
        }
        // return launched process pid, pipeline process, and the descriptors
        // the redirections own (the caller must not close those itself)
        Ok(ProcessLaunchOutcome::Launched {
            pid,
            next_process,
            redirects: applied,
        })
    }

    /// Close and unwind the pipe/capture wiring created by [`JobProcess::launch`]
    /// after a redirection failure, before anything spawned.
    ///
    /// Only descriptors created by that call are closed: `pipe_out` (the read
    /// end for the next stage), `created_out` / `created_err` (fresh pipe
    /// write ends now living in `ctx`), and the capture-reader ends stashed
    /// on the process. Everything else in `ctx` is caller-owned (the caller's
    /// capture pipe, the base stdio) or job-owned (the PTY slave, only
    /// unwound, never closed), so the slots are simply restored to the entry
    /// values. No slot comparison is involved: a pipeline stage's entry
    /// `ctx.outfile` is the previous stage's already-closed pipe write end,
    /// which a fresh pipe could never be distinguished from by number alone.
    fn abort_stage_wiring(
        &mut self,
        ctx: &mut Context,
        entry: (RawFd, RawFd, RawFd),
        pipe_out: Option<RawFd>,
        created_out: Option<RawFd>,
        created_err: Option<RawFd>,
    ) {
        if let Some(read_end) = pipe_out {
            let _ = close(read_end);
        }
        if let Some(write_end) = created_out {
            let _ = close(write_end);
        }
        if let Some(write_end) = created_err {
            let _ = close(write_end);
        }
        // Capture-reader ends stashed on the process never reached a spawn;
        // take them back and close so they cannot leak.
        let (cap_stdout, cap_stderr) = match self {
            JobProcess::Builtin(process) => (process.cap_stdout.take(), process.cap_stderr.take()),
            JobProcess::Command(process) => (process.cap_stdout.take(), process.cap_stderr.take()),
            JobProcess::SyntheticSource(_) => (None, None),
        };
        if let Some(fd) = cap_stdout {
            let _ = close(fd);
        }
        if let Some(fd) = cap_stderr {
            let _ = close(fd);
        }
        // `redirect::apply` already rolled its own partial changes back, so
        // the slots now hold the post-wiring values; put back exactly what
        // the caller handed us.
        ctx.infile = entry.0;
        ctx.outfile = entry.1;
        ctx.errfile = entry.2;
    }

    pub fn kill(&self) -> Result<()> {
        use super::signal::send_signal_allow_gone;

        let Some(pid) = self.owned_child_pid(getpid()) else {
            return Ok(());
        };
        if matches!(self.get_state(), ProcessState::Completed(_, _)) {
            return Ok(());
        }
        send_signal_allow_gone(pid, Signal::SIGKILL)
    }

    pub fn cont(&self) -> Result<()> {
        use super::signal::send_signal_allow_gone;

        let Some(pid) = self.owned_child_pid(getpid()) else {
            return Ok(());
        };
        if matches!(self.get_state(), ProcessState::Completed(_, _)) {
            return Ok(());
        }
        debug!("send signal SIGCONT pid:{:?}", pid);
        send_signal_allow_gone(pid, Signal::SIGCONT)
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        match self {
            JobProcess::Builtin(process) => process.update_state(),
            JobProcess::Command(process) => process.update_state(),
            JobProcess::SyntheticSource(process) => process.update_state(),
        }
    }
}
