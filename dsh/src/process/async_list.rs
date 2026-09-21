//! One asynchronous AND-OR list as a managed background job.
//!
//! `&` separates whole AND-OR lists, so `a && b &` spawns one helper that
//! evaluates `a && b` as an ordinary foreground chain in its own shell:
//! `cd` and assignments persist across the list inside the helper but never
//! leak into the parent. The helper pid, its process group, and its capture
//! pipes live in the normal `Job` lifecycle (`wait_jobs`, `OutputMonitor`,
//! group-kill on shutdown) — never as a detached spawn.

use super::io::{OutputMonitor, cloexec_pipe};
use super::job_process::JobProcess;
use super::reexec::{ChildStdio, PlanExecMode, spawn_plan_helper};
use super::state::ProcessState;
use super::wait::{WaitPidObservation, wait_pid_job};
use crate::shell::plan::ExecutionPlan;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::{Pid, getpid};
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::os::unix::io::RawFd;

/// A background AND-OR list: source text, the foreground-normalized body
/// plan, and the canonical lifecycle state of its helper child.
#[derive(Clone, PartialEq, Eq)]
pub struct AsyncListProcess {
    pub(crate) source: String,
    pub(crate) plan: ExecutionPlan,
    pub(crate) state: ProcessState,
    pub(crate) pid: Option<Pid>,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub(crate) cap_stdout: Option<RawFd>,
    pub(crate) cap_stderr: Option<RawFd>,
}

impl std::fmt::Debug for AsyncListProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncListProcess")
            .field("source", &self.source)
            .field("state", &self.state)
            .field("pid", &self.pid)
            .field("has_next", &self.next.is_some())
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl AsyncListProcess {
    pub fn new(source: String, plan: ExecutionPlan) -> Self {
        Self {
            source,
            plan,
            state: ProcessState::Running,
            pid: None,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            cap_stdout: None,
            cap_stderr: None,
        }
    }

    pub fn get_state(&self) -> ProcessState {
        self.state
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(self_pid) = self.pid
            && self_pid == pid
        {
            self.state = state;
            return true;
        }
        if let Some(ref mut next) = self.next
            && next.set_state_pid(pid, state)
        {
            return true;
        }
        false
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => p.link(process),
            None => self.next = Some(Box::new(process)),
        }
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        // Same lifecycle as external/re-exec children: only an observed
        // `waitpid` status enters the tree. A still-running helper stays
        // `Running`; ECHILD (reaped elsewhere) keeps existing state.
        if !matches!(self.state, ProcessState::Completed(_, _))
            && let Some(pid) = self.pid
            && pid != getpid()
        {
            match wait_pid_job(pid, true) {
                Ok(WaitPidObservation::State(_, state)) => {
                    self.state = state;
                }
                Ok(WaitPidObservation::StillAlive) => {}
                Ok(WaitPidObservation::NoChild) => {}
                Err(nix::errno::Errno::EINTR) => {}
                Err(err) => {
                    tracing::debug!(
                        "async list update_state: waitpid for pid {} failed: {}; keeping {:?}",
                        pid,
                        err,
                        self.state
                    );
                }
            }
        }
        if let Some(next) = self.next.as_mut() {
            next.update_state();
        }
        Some(self.state)
    }
}

/// Spawn the helper for one async list and wire it into `job`.
///
/// Returns the helper pid. The caller records group ownership and pushes
/// the job onto `wait_jobs`; completion flows through the shared
/// wait/reap/kill machinery afterwards.
///
/// FD ownership: every pipe end created here is either handed to the
/// helper (inherited at spawn, closed in the parent right after) or moved
/// into the job's `OutputMonitor`s. A spawn failure closes everything it
/// created and reports `Err` with no zombie (the spawn itself either
/// failed, leaving no child, or `spawn_internal_helper` reaped the child
/// after an undeliverable payload).
pub(crate) fn spawn_async_list(
    process: &mut AsyncListProcess,
    ctx: &mut Context,
    shell: &crate::shell::Shell,
    job: &mut super::job::Job,
) -> Result<Pid> {
    // Without job control the helper must not inherit the terminal stdin:
    // it starts on /dev/null. An explicit `< file` redirection inside the
    // list overrides this later, in the helper itself. The `File` owner
    // stays alive on this stack until the spawn below returns, then drops.
    use std::os::fd::AsRawFd as _;
    let _null_stdin: Option<std::fs::File>;
    let helper_stdin = if !ctx.supports_job_control() {
        let null = std::fs::File::open("/dev/null").context("open /dev/null for async stdin")?;
        let fd = null.as_raw_fd();
        _null_stdin = Some(null);
        fd
    } else {
        _null_stdin = None;
        ctx.infile
    };
    // Same background-capture contract as external commands: when stdout
    // (stderr) still names the terminal, route the helper through a pipe
    // into an `OutputMonitor` instead of interleaving on the terminal.
    let mut held_write_ends: Vec<std::os::fd::OwnedFd> = Vec::new();
    let (helper_stdout, cap_stdout) =
        capture_or_direct(ctx.outfile, STDOUT_FILENO, &mut held_write_ends)?;
    let (helper_stderr, cap_stderr) =
        capture_or_direct(ctx.errfile, STDERR_FILENO, &mut held_write_ends)?;
    process.cap_stdout = cap_stdout;
    process.cap_stderr = cap_stderr;

    let snapshot =
        crate::environment::child_snapshot::ChildShellSnapshot::capture(&shell.environment.read());
    // A top-level list (`ctx.pgid` unset) founds a fresh process group so
    // shutdown can group-kill the helper and everything it started. A
    // nested list inside a helper joins the existing async group instead.
    let fresh_group = ctx.pgid.is_none();
    let pgroup = ctx.pgid.unwrap_or(Pid::from_raw(0));
    let child = match spawn_plan_helper(
        &snapshot,
        &process.plan,
        PlanExecMode::AsyncAndOrList,
        ChildStdio {
            stdin: helper_stdin,
            stdout: helper_stdout,
            stderr: helper_stderr,
        },
        pgroup,
        None,
    ) {
        Ok(child) => child,
        Err(err) => {
            // The capture readers never reached a monitor: close them here
            // so a spawn failure leaks no fd. Write ends and `/dev/null`
            // drop via their owners; the spawn itself left no child behind
            // (or reaped it — see `spawn_internal_helper`).
            if let Some(fd) = process.cap_stdout.take() {
                let _ = nix::unistd::close(fd);
            }
            if let Some(fd) = process.cap_stderr.take() {
                let _ = nix::unistd::close(fd);
            }
            return Err(err);
        }
    };
    // The helper inherited the stdio and capture write ends at spawn;
    // closing the parent copies lets readers see EOF when it exits.
    drop(held_write_ends);

    // `helper_stdin` borrows `_null_stdin`, which closes at spawn return:
    // store it only while the owner outlives this function (the job-control
    // path, where `ctx.infile` is caller-owned). Otherwise record the
    // conventional value rather than a dangling number — nothing launches
    // from this field afterwards.
    process.stdin = if _null_stdin.is_some() {
        STDIN_FILENO
    } else {
        helper_stdin
    };
    process.stdout = helper_stdout;
    process.stderr = helper_stderr;
    process.pid = Some(child);
    ctx.process_count += 1;

    if fresh_group {
        job.pgid = Some(child);
        ctx.pgid = Some(child);
    }
    if job.pid.is_none() {
        job.pid = Some(child);
    }
    attach_monitors(process, ctx, job);
    Ok(child)
}

/// Launch one async AND-OR list as a single managed helper.
///
/// Called from `Job::launch_process` before the per-stage pipeline wiring:
/// an async list owns its stdin default, capture pipes, and process-group
/// bookkeeping here, so the generic path below would double-capture it.
pub(crate) fn launch_async_list_process(
    job: &mut super::job::Job,
    ctx: &mut Context,
    shell: &crate::shell::Shell,
    process: &mut JobProcess,
) -> Result<super::launch_outcome::StageLaunchOutcome> {
    let JobProcess::AsyncList(async_process) = process else {
        anyhow::bail!("async list launcher called on a non-async process");
    };
    spawn_async_list(async_process, ctx, shell, job)?;
    job.state = async_process.state;
    job.set_process(process.to_owned());
    Ok(super::launch_outcome::StageLaunchOutcome::Launched)
}

/// One stdio slot: a fresh capture pipe (returning the helper-side write
/// end plus the parent-side read end) when the slot still names the
/// terminal, otherwise the slot itself.
fn capture_or_direct(
    slot: RawFd,
    terminal_fd: RawFd,
    held_write_ends: &mut Vec<std::os::fd::OwnedFd>,
) -> Result<(RawFd, Option<RawFd>)> {
    if slot == terminal_fd {
        let (read, write) = cloexec_pipe().context("async list capture pipe")?;
        let read_fd = read.into_raw_fd();
        let write_fd = write.as_raw_fd();
        held_write_ends.push(write);
        Ok((write_fd, Some(read_fd)))
    } else {
        Ok((slot, None))
    }
}

/// By construction an async list carries no per-node redirects or env
/// overrides (redirections live on the list's own stages, evaluated inside
/// the helper); dropping them silently would be fail-open, so log loudly in
/// release too (`debug_assert` only fires in dev).
pub(crate) fn assert_no_async_list_redirects(redirects_len: usize) {
    debug_assert!(redirects_len == 0, "async list takes no redirects");
    if redirects_len != 0 {
        tracing::error!("async list ignoring unexpected redirects");
    }
}

/// Same fail-closed guard for `NAME=value` overrides (see above).
pub(crate) fn assert_no_async_list_env(overrides_len: usize) {
    debug_assert!(overrides_len == 0, "async list takes no env");
    if overrides_len != 0 {
        tracing::error!("async list ignoring unexpected env overrides");
    }
}

/// Move fresh capture readers into `OutputMonitor`s owned by the job.
fn attach_monitors(process: &AsyncListProcess, ctx: &Context, job: &mut super::job::Job) {
    use dsh_types::observed_output::ObservedStream;
    if let Some(stdout) = process.cap_stdout {
        job.monitors.push(OutputMonitor::new(
            stdout,
            ctx.output_observer.clone(),
            ObservedStream::Stdout,
        ));
    }
    if let Some(stderr) = process.cap_stderr {
        job.monitors.push(OutputMonitor::new(
            stderr,
            ctx.output_observer.clone(),
            ObservedStream::Stderr,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::JobProcess;
    use crate::shell::plan::{ExecutionPlan, ListExecutionMode, PlannedAndOrList};
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;

    fn empty_body(source: &str) -> ExecutionPlan {
        ExecutionPlan {
            lists: vec![PlannedAndOrList {
                source: source.to_string(),
                jobs: Vec::new(),
                execution: ListExecutionMode::Foreground,
            }],
        }
    }

    #[test]
    fn async_list_starts_running_without_child() {
        let process = AsyncListProcess::new("a && b &".to_string(), empty_body("a && b"));
        assert_eq!(process.state, ProcessState::Running);
        assert_eq!(process.pid, None);
        assert_eq!(process.get_state(), ProcessState::Running);
        // Not a command: the safety projection skips it.
        let node = JobProcess::AsyncList(process);
        assert_eq!(node.command_argv(), None);
        assert!(node.redirects().is_empty());
    }

    #[test]
    fn async_list_records_waitpid_state() {
        let mut process = AsyncListProcess::new("a &".to_string(), empty_body("a"));
        let pid = Pid::from_raw(424242);
        process.pid = Some(pid);
        assert!(process.set_state(pid, ProcessState::Completed(3, None)));
        assert_eq!(process.get_state(), ProcessState::Completed(3, None));
        // Unknown pids never touch the node.
        assert!(!process.set_state(Pid::from_raw(1), ProcessState::Completed(0, None)));
        assert_eq!(process.get_state(), ProcessState::Completed(3, None));
    }

    #[test]
    fn async_list_update_state_never_waits_on_shell_pid() {
        // A node without a real child (or one carrying the shell pid, as
        // in-process builtins do) stays Running: ECHILD invents no status.
        let mut process = AsyncListProcess::new("a &".to_string(), empty_body("a"));
        process.pid = Some(getpid());
        let state = process
            .update_state()
            .expect("async update_state always returns the node state");
        assert_eq!(state, ProcessState::Running);
    }

    #[test]
    fn async_list_kill_targets_owned_child_only() {
        // No child, or an already-completed one: kill is a no-op success.
        let node = JobProcess::AsyncList(AsyncListProcess::new("a &".to_string(), empty_body("a")));
        node.kill().expect("kill without child is a no-op");
        let mut done = AsyncListProcess::new("a &".to_string(), empty_body("a"));
        done.pid = Some(Pid::from_raw(424243));
        done.state = ProcessState::Completed(0, None);
        JobProcess::AsyncList(done)
            .kill()
            .expect("kill on completed state is a no-op");
    }

    /// Spawn a real helper through `spawn_async_list`: fresh process group
    /// recorded on the job, helper output captured, completion observed via
    /// `waitpid` (never synthesized).
    ///
    /// Needs the sibling `dogesh` binary: run a full `cargo test -p
    /// doge-shell` first, like the other re-exec tests.
    #[tokio::test]
    async fn async_list_spawn_records_fresh_group_and_captures_output() {
        use crate::environment::Environment;
        use crate::shell::Shell;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let env = Environment::new();
        let shell = Shell::new(env.clone());
        let plan =
            crate::shell::parse::parse_execution_plan("echo async-list-marker", Arc::clone(&env))
                .expect("parse async body");
        assert_eq!(plan.lists.len(), 1);
        let list = &plan.lists[0];
        let mut process = AsyncListProcess::new(list.display_source(), list.isolated_body_plan());

        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        ctx.foreground = false;
        ctx.interactive = false;
        let mut job =
            crate::process::job::Job::new("echo async-list-marker &".to_string(), shell.pgid);
        let child = match spawn_async_list(&mut process, &mut ctx, &shell, &mut job) {
            Ok(pid) => pid,
            Err(err) => panic!("dogesh helper binary missing for async spawn test: {err:#}"),
        };
        assert_eq!(process.pid, Some(child));
        // A top-level list founds a fresh group led by the helper itself.
        assert_eq!(job.pgid, Some(child));
        assert_eq!(job.pid, Some(child));
        // Background capture for both streams, like external background
        // commands: no double capture, no direct terminal wiring.
        assert_eq!(job.monitors.len(), 2);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = process
                .update_state()
                .expect("async update_state always returns the node state");
            if matches!(state, ProcessState::Completed(0, None)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "async helper never completed: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for monitor in job.monitors.iter_mut() {
            monitor.drain_to_eof().await.expect("drain helper output");
        }
        let captured: String = job
            .monitors
            .iter()
            .map(|monitor| monitor.captured_output.clone())
            .collect();
        assert!(
            captured.contains("async-list-marker"),
            "helper output missing from capture: {captured:?}"
        );
    }

    /// A running async helper is killable through the node: the signal
    /// reaches the owned child (fresh group leader) and `waitpid`
    /// observation records the signal death — never synthesized status.
    #[tokio::test]
    async fn async_list_kill_reaches_running_helper() {
        use crate::environment::Environment;
        use crate::shell::Shell;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let env = Environment::new();
        let shell = Shell::new(env.clone());
        let plan = crate::shell::parse::parse_execution_plan("sleep 30", Arc::clone(&env))
            .expect("parse async body");
        let list = &plan.lists[0];
        let mut process = AsyncListProcess::new(list.display_source(), list.isolated_body_plan());

        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        ctx.foreground = false;
        ctx.interactive = false;
        let mut job = crate::process::job::Job::new("sleep 30 &".to_string(), shell.pgid);
        let child = match spawn_async_list(&mut process, &mut ctx, &shell, &mut job) {
            Ok(pid) => pid,
            Err(err) => panic!("dogesh helper binary missing for async kill test: {err:#}"),
        };
        assert_ne!(child, getpid());
        JobProcess::AsyncList(process.clone())
            .kill()
            .expect("kill running helper");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = process
                .update_state()
                .expect("async update_state always returns the node state");
            if let ProcessState::Completed(_, Some(Signal::SIGKILL)) = state {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "killed helper never reaped as SIGKILL: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
