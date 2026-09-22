use anyhow::Result;
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::Signal;
use nix::unistd::{Pid, getpid};
use std::os::unix::io::RawFd;
use tracing::debug;

use super::async_list::AsyncListProcess;
use super::builtin::BuiltinProcess;
use super::launch_outcome::CommandFailure;
use super::no_command_process::NoCommandProcess;
use super::pipeline_source::PipelineSourceProcess;
use super::process::Process;
use super::pty::PtyMode;
use super::redirect::{AppliedRedirects, Redirect};
use super::state::ProcessState;
use dsh_types::Context;

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Command(Process),
    SyntheticSource(PipelineSourceProcess),
    AsyncList(AsyncListProcess),
    NoCommand(NoCommandProcess),
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
            JobProcess::AsyncList(jprocess) => f
                .debug_struct("JobProcess::AsyncList")
                .field("source", &jprocess.source)
                .field("pid", &jprocess.pid)
                .field("state", &jprocess.state)
                .finish(),
            // Assignment values may carry secrets: counts only, never values.
            JobProcess::NoCommand(jprocess) => f
                .debug_struct("JobProcess::NoCommand")
                .field("assignments_len", &jprocess.assignments.len())
                .field("redirects_len", &jprocess.redirects.len())
                .field("pid", &jprocess.pid)
                .field("state", &jprocess.state)
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
/// `Launched` carries the child pid, the detached downstream stages, the
/// redirection guard (the caller owns its files and must not close those
/// descriptors itself), and the pre-spawn capture monitors (moved once into
/// `Job.monitors` by the caller). `CommandFailed` is a redirection setup
/// failure: the stage never spawned, its temporary pipe/capture wiring is
/// already closed and unwound, and the evaluator reports it as an ordinary
/// command failure. Only internal failures (pipe creation, monitor setup,
/// spawn protocol) travel as `Err`.
#[derive(Debug)]
pub(crate) enum ProcessLaunchOutcome {
    Launched {
        pid: Pid,
        next_process: Option<Box<JobProcess>>,
        redirects: AppliedRedirects,
        monitors: Vec<super::io::OutputMonitor>,
    },
    CommandFailed(CommandFailure),
}

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
            JobProcess::SyntheticSource(jprocess) => jprocess.link(process),
            JobProcess::AsyncList(jprocess) => jprocess.link(process),
            JobProcess::NoCommand(jprocess) => jprocess.link(process),
        }
    }

    /// The next stage of the pipeline, borrowed rather than cloned.
    pub(crate) fn next_process(&self) -> Option<&JobProcess> {
        match self {
            JobProcess::Builtin(p) => p.next.as_deref(),
            JobProcess::Command(p) => p.next.as_deref(),
            JobProcess::SyntheticSource(p) => p.next.as_deref(),
            JobProcess::AsyncList(p) => p.next.as_deref(),
            JobProcess::NoCommand(p) => p.next.as_deref(),
        }
    }

    /// The next stage of the pipeline, borrowed mutably.
    pub(crate) fn next_process_mut(&mut self) -> Option<&mut JobProcess> {
        match self {
            JobProcess::Builtin(p) => p.next.as_deref_mut(),
            JobProcess::Command(p) => p.next.as_deref_mut(),
            JobProcess::SyntheticSource(p) => p.next.as_deref_mut(),
            JobProcess::AsyncList(p) => p.next.as_deref_mut(),
            JobProcess::NoCommand(p) => p.next.as_deref_mut(),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::SyntheticSource(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::AsyncList(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::NoCommand(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn mut_next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::SyntheticSource(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::AsyncList(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::NoCommand(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn take_next(&mut self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.take(),
            JobProcess::Command(jprocess) => jprocess.next.take(),
            JobProcess::SyntheticSource(jprocess) => jprocess.next.take(),
            JobProcess::AsyncList(jprocess) => jprocess.next.take(),
            JobProcess::NoCommand(jprocess) => jprocess.next.take(),
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
            JobProcess::AsyncList(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::NoCommand(jprocess) => {
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
            JobProcess::AsyncList(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::NoCommand(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
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
            JobProcess::AsyncList(process) => {
                process.pid = pid;
            }
            JobProcess::NoCommand(process) => {
                process.pid = pid;
            }
        }
    }

    pub fn get_pid(&self) -> Option<Pid> {
        match self {
            JobProcess::Builtin(process) => process.pid,
            JobProcess::Command(process) => process.pid,
            JobProcess::SyntheticSource(process) => process.pid,
            JobProcess::AsyncList(process) => process.pid,
            JobProcess::NoCommand(process) => process.pid,
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
            JobProcess::AsyncList(p) => p.state = state,
            JobProcess::NoCommand(p) => p.state = state,
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
            JobProcess::AsyncList(p) => {
                debug!("🔄 STATE: Setting state for async list");
                p.set_state(pid, state)
            }
            JobProcess::NoCommand(p) => {
                debug!("🔄 STATE: Setting state for no-command stage");
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
            JobProcess::AsyncList(p) => p.state,
            JobProcess::NoCommand(p) => p.state,
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
            JobProcess::AsyncList(process) => {
                if matches!(process.state, ProcessState::Stopped(_, _)) {
                    process.state = ProcessState::Running;
                }
                if let Some(next) = process.next.as_deref_mut() {
                    next.mark_stopped_processes_running();
                }
            }
            JobProcess::NoCommand(process) => {
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

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
            JobProcess::Command(p) => &p.cmd,
            JobProcess::SyntheticSource(_) => "<smart-pipe-source>",
            // User-facing command identity lives in `Job.cmd`; the stage
            // label here must not rebuild source text from assignments.
            JobProcess::NoCommand(_) => "<no-command>",
            JobProcess::AsyncList(p) => &p.source,
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
            // Neither a synthetic source, an async list, nor a no-command
            // stage is a classifiable command: the guard skips all three.
            // A no-command stage carries no argv; substitutions inside its
            // assignments already passed authorization at materialization.
            JobProcess::SyntheticSource(_)
            | JobProcess::AsyncList(_)
            | JobProcess::NoCommand(_) => None,
        }
    }

    /// Redirections written on this command.
    pub(crate) fn redirects(&self) -> &[Redirect] {
        static EMPTY_REDIRECTS: &[Redirect] = &[];
        match self {
            JobProcess::Builtin(p) => &p.redirects,
            JobProcess::Command(p) => &p.redirects,
            // A no-command stage carries real simple-command redirections,
            // applied exactly once by the normal pipeline wiring.
            JobProcess::NoCommand(p) => &p.redirects,
            JobProcess::SyntheticSource(_) | JobProcess::AsyncList(_) => EMPTY_REDIRECTS,
        }
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
            JobProcess::AsyncList(process) => process.update_state(),
            JobProcess::NoCommand(process) => process.update_state(),
        }
    }
}
