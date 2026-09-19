use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::Signal;
use nix::unistd::{Pid, close, getpid};
use std::os::fd::IntoRawFd;
use std::os::unix::io::RawFd;
use tracing::debug;

use super::builtin::BuiltinProcess;
use super::fork::fork_process;
use super::io::{cloexec_pipe, create_pipe, default_output_wiring};
use super::process::Process;
use super::pty::{PtyChildConfig, PtyMode};
use super::redirect::{self, AppliedRedirects, Redirect};
use super::reexec::spawn_background_builtin;
use super::signal::send_signal;
use super::state::ProcessState;
use crate::shell::Shell;
use dsh_types::Context;

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Command(Process),
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
        }
    }
}

fn apply_pty_stdio(ctx: &mut Context, slave: RawFd, pty_mode: PtyMode) -> bool {
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

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
        }
    }

    /// The next stage of the pipeline, borrowed rather than cloned.
    pub(crate) fn next_process(&self) -> Option<&JobProcess> {
        match self {
            JobProcess::Builtin(p) => p.next.as_deref(),
            JobProcess::Command(p) => p.next.as_deref(),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn mut_next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn take_next(&mut self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.take(),
            JobProcess::Command(jprocess) => jprocess.next.take(),
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
        }
    }

    pub fn get_io(&self) -> (RawFd, RawFd, RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(_) => {
                // noop
            }
            JobProcess::Command(process) => {
                process.pid = pid;
            }
        }
    }

    pub fn get_pid(&self) -> Option<Pid> {
        match self {
            JobProcess::Builtin(_) => {
                // noop
                None
            }
            JobProcess::Command(process) => process.pid,
        }
    }

    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
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
        };
        debug!("🔄 STATE: set_state_pid result: {}", result);
        result
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
            JobProcess::Command(p) => p.state,
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
        }
    }

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
            JobProcess::Command(p) => &p.cmd,
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
    pub(crate) fn command_argv(&self) -> (&str, &[String]) {
        match self {
            JobProcess::Builtin(p) => (p.name.as_str(), strip_argv0(&p.name, &p.argv)),
            JobProcess::Command(p) => (p.cmd.as_str(), strip_argv0(&p.cmd, &p.argv)),
        }
    }

    pub fn waitable(&self) -> bool {
        matches!(self, JobProcess::Command(_))
    }

    /// Redirections written on this command.
    pub(crate) fn redirects(&self) -> &[Redirect] {
        match self {
            JobProcess::Builtin(p) => &p.redirects,
            JobProcess::Command(p) => &p.redirects,
        }
    }

    pub(crate) fn set_redirects(&mut self, redirects: Vec<Redirect>) {
        match self {
            JobProcess::Builtin(p) => p.redirects = redirects,
            JobProcess::Command(p) => p.redirects = redirects,
        }
    }

    pub(crate) fn set_env_overrides(&mut self, overrides: Vec<(String, String)>) {
        match self {
            JobProcess::Builtin(p) => p.env_overrides = overrides,
            JobProcess::Command(p) => p.env_overrides = overrides,
        }
    }

    pub(crate) async fn launch(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        stdout: RawFd,
        pty: Option<PtyChildConfig>,
    ) -> Result<(Pid, Option<Box<JobProcess>>, AppliedRedirects)> {
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

        let pipe_out = match next_process {
            Some(_) => {
                create_pipe(ctx)? // create pipe
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
                    let pout_raw = pout.into_raw_fd();
                    match self {
                        JobProcess::Builtin(p) => p.cap_stdout = Some(pout_raw),
                        JobProcess::Command(p) => p.cap_stdout = Some(pout_raw),
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

        let applied = redirect::apply(&output_redirects, ctx)?;

        self.set_io(ctx.infile, ctx.outfile, ctx.errfile);

        // initial pid
        let current_pid = getpid();

        let launched: Result<Pid> = async {
            Ok(match self {
                JobProcess::Builtin(process) => {
                    if ctx.foreground {
                        process.pid = Some(current_pid);
                        process.launch(ctx, shell).await?;
                        current_pid
                    } else {
                        // Background builtins re-exec into a fresh helper
                        // process (no fork-copy, no post-fork Rust).
                        let child_pid = spawn_background_builtin(ctx, process, shell)?;
                        process.pid = Some(child_pid);
                        child_pid
                    }
                }
                JobProcess::Command(process) => {
                    ctx.process_count += 1;
                    // fork
                    fork_process(ctx, ctx.pgid, process, shell, pty)?
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
        Ok((pid, next_process, applied))
    }

    pub fn kill(&self) -> Result<()> {
        match self {
            JobProcess::Builtin(_) => Ok(()),
            JobProcess::Command(process) => {
                if let Some(pid) = process.pid {
                    send_signal(pid, Signal::SIGKILL)
                } else {
                    Ok(())
                }
            }
        }
    }

    pub fn cont(&self) -> Result<()> {
        match self {
            JobProcess::Builtin(_) => Ok(()),
            JobProcess::Command(process) => {
                if let Some(pid) = process.pid {
                    debug!("send signal SIGCONT pid:{:?}", pid);
                    send_signal(pid, Signal::SIGCONT)
                } else {
                    Ok(())
                }
            }
        }
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        match self {
            JobProcess::Builtin(process) => process.update_state(),
            JobProcess::Command(process) => process.update_state(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::state::ProcessState;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn running_producer_with_completed_consumer_is_not_tree_completed() {
        init();

        // Create a pipeline: cat | less
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set initial states: producer running, consumer completed.
        cat_process.state = ProcessState::Running;
        less_process.state = ProcessState::Completed(0, None);

        // Link them in pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process)));

        let cat_job_process = JobProcess::Command(cat_process);

        // Strict tree completion: a completed final stage alone is not
        // completion while the producer is still running.
        assert!(!cat_job_process.is_completed());
    }

    #[test]
    fn completed_process_is_not_stopped() {
        init();
        let mut process = Process::new("test".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);

        let pipeline = JobProcess::Command(process);
        assert!(!pipeline.has_stopped_process());
        assert!(!pipeline.is_fully_stopped());
    }

    fn pipeline_with_states(states: &[ProcessState]) -> JobProcess {
        let mut states = states.iter().copied();
        let first = states.next().expect("pipeline needs at least one stage");
        let mut process = Process::new("stage-1".to_string(), vec![]);
        process.state = first;
        let mut pipeline = JobProcess::Command(process);
        for (index, state) in states.enumerate() {
            let mut process = Process::new(format!("stage-{}", index + 2), vec![]);
            process.state = state;
            pipeline.link(JobProcess::Command(process));
        }
        pipeline
    }

    fn pipeline_states(process: &JobProcess) -> Vec<ProcessState> {
        let mut states = Vec::new();
        let mut current = Some(process);
        while let Some(process) = current {
            states.push(process.get_state());
            current = process.next_process();
        }
        states
    }

    #[test]
    fn stopped_query_sees_stopped_tail_behind_running_pipeline_head() {
        let pipeline = pipeline_with_states(&[
            ProcessState::Running,
            ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
        ]);

        assert!(pipeline.has_stopped_process());
    }

    /// Strict tree completion: only all-`Completed` pipelines complete.
    /// A completed final stage alone (or a successful intermediate stage)
    /// never completes the tree.
    #[test]
    fn pipeline_tree_completion_truth_table() {
        use ProcessState::{Completed, Running, Stopped};
        let stopped = || Stopped(Pid::from_raw(12), Signal::SIGTSTP);
        let signaled = || Completed(0, Some(Signal::SIGPIPE));
        let cases: &[(&[ProcessState], bool)] = &[
            (&[Running], false),
            (&[Completed(0, None)], true),
            (&[Running, Running], false),
            (&[Running, Completed(0, None)], false),
            (&[Running, Completed(1, None)], false),
            (&[Running, signaled()], false),
            (&[Running, Completed(0, None), Running], false),
            (&[Running, Completed(0, None), stopped()], false),
            (&[Running, Running, Completed(0, None)], false),
            (&[Completed(0, None), Completed(0, None), Running], false),
            (&[Completed(0, None), Running, Completed(0, None)], false),
            (
                &[Completed(0, None), Completed(0, None), Completed(0, None)],
                true,
            ),
            (
                &[Completed(3, None), Completed(0, None)],
                // Non-zero upstream still counts as completed: exit codes
                // never affect tree completion.
                true,
            ),
            (&[Completed(1, None), Completed(0, None), Running], false),
            (&[Completed(1, None), Running, Completed(0, None)], false),
        ];
        for (states, expected) in cases {
            let pipeline = pipeline_with_states(states);
            assert_eq!(
                pipeline.is_completed(),
                *expected,
                "tree completion for {:?}",
                states,
            );
        }
    }

    #[test]
    fn mark_stopped_processes_running_updates_single_process() {
        let mut process =
            pipeline_with_states(&[ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP)]);

        process.mark_stopped_processes_running();

        assert_eq!(pipeline_states(&process), vec![ProcessState::Running]);
    }

    #[test]
    fn mark_stopped_processes_running_updates_all_stopped_pipeline_stages() {
        let mut process = pipeline_with_states(&[
            ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
            ProcessState::Stopped(Pid::from_raw(12), Signal::SIGSTOP),
            ProcessState::Stopped(Pid::from_raw(13), Signal::SIGTTIN),
        ]);

        process.mark_stopped_processes_running();

        assert_eq!(
            pipeline_states(&process),
            vec![
                ProcessState::Running,
                ProcessState::Running,
                ProcessState::Running
            ]
        );
    }

    #[test]
    fn mark_stopped_processes_running_preserves_completed_pipeline_stage() {
        let mut process = pipeline_with_states(&[
            ProcessState::Completed(0, None),
            ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
            ProcessState::Running,
        ]);

        process.mark_stopped_processes_running();

        assert_eq!(
            pipeline_states(&process),
            vec![
                ProcessState::Completed(0, None),
                ProcessState::Running,
                ProcessState::Running
            ]
        );
    }

    #[test]
    fn test_job_process_variants() {
        init();
        let process = Process::new("test".to_string(), vec![]);
        let job_process = JobProcess::Command(process);

        // JobProcess type check
        match job_process {
            JobProcess::Command(_) => {} // Expected variant
            _ => panic!("Expected Command variant"),
        }
    }

    #[test]
    fn output_only_pty_keeps_stdin_on_real_terminal() {
        let mut ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true);
        let slave = 42;

        let applied = apply_pty_stdio(&mut ctx, slave, PtyMode::OutputOnly);

        assert!(applied);
        assert_eq!(ctx.infile, STDIN_FILENO);
        assert_eq!(ctx.outfile, slave);
        assert_eq!(ctx.errfile, slave);
    }

    #[test]
    fn full_proxy_pty_replaces_stdin_stdout_and_stderr() {
        let mut ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true);
        let slave = 42;

        let applied = apply_pty_stdio(&mut ctx, slave, PtyMode::FullProxy);

        assert!(applied);
        assert_eq!(ctx.infile, slave);
        assert_eq!(ctx.outfile, slave);
        assert_eq!(ctx.errfile, slave);
    }
}
