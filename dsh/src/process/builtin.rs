//! Foreground in-process and background re-exec builtin pipeline nodes.
//!
//! A `BuiltinProcess` with `pid == getpid()` runs inside the shell and is
//! never waited on or signaled; a background node owns a real `posix_spawn`
//! helper child and shares the external wait/reap/kill lifecycle, polled
//! through the shared `wait_pid_job` decoder.

use anyhow::Result;
use dsh_builtin::BuiltinHandler;
use dsh_types::{Context, ExitStatus};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::Pid;
use std::os::unix::io::RawFd;
use tracing::debug;

use super::job_process::JobProcess;
use super::redirect::Redirect;
use super::state::ProcessState;
use super::wait::{WaitPidObservation, wait_pid_job};
use crate::shell::Shell;
use nix::unistd::getpid;

/// Where a builtin stage executes.
///
/// Only a single-stage foreground builtin runs in the parent shell. Every
/// other placement — background, or any member of a multi-stage pipeline
/// (first/middle/last) — re-execs into an isolated helper child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinExecutionPlacement {
    Parent,
    Reexec,
}

/// Single decision point for builtin execution placement.
///
/// `foreground` is the job's foreground flag; `pipeline_context` is true
/// when the job holds more than one pipeline stage (a synthetic Smart Pipe
/// source counts as a stage).
pub fn builtin_execution_placement(
    foreground: bool,
    pipeline_context: bool,
) -> BuiltinExecutionPlacement {
    if foreground && !pipeline_context {
        BuiltinExecutionPlacement::Parent
    } else {
        BuiltinExecutionPlacement::Reexec
    }
}

#[derive(Clone)]
pub struct BuiltinProcess {
    pub(crate) name: String,
    pub(crate) handler: BuiltinHandler,
    pub(crate) argv: Vec<String>,
    pub(crate) state: ProcessState, // completed, stopped,
    pub pid: Option<Pid>,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub(crate) cap_stdout: Option<RawFd>,
    pub(crate) cap_stderr: Option<RawFd>,
    /// Redirections written on *this* command, in order. Per process,
    /// not per job: in `a 2>&1 | b` the duplication belongs to `a`.
    pub(crate) redirects: Vec<Redirect>,
    /// `NAME=value` written before this command; visible to it only.
    pub(crate) env_overrides: Vec<(String, String)>,
}

impl PartialEq for BuiltinProcess {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv
    }
}

impl Eq for BuiltinProcess {}

impl std::fmt::Debug for BuiltinProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("BuiltinProcess")
            .field("argv", &self.argv)
            .field("state", &self.state)
            .field("pid", &self.pid)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl BuiltinProcess {
    pub fn new(
        name: String,
        cmd_fn: fn(&Context, Vec<String>, &mut dyn dsh_builtin::ShellProxy) -> ExitStatus,
        argv: Vec<String>,
    ) -> Self {
        Self::new_handler(name, BuiltinHandler::Sync(cmd_fn), argv)
    }

    pub fn new_handler(name: String, handler: BuiltinHandler, argv: Vec<String>) -> Self {
        BuiltinProcess {
            name,
            handler,
            argv,
            state: ProcessState::Running,
            pid: None,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            cap_stdout: None,
            cap_stderr: None,
            redirects: Vec::new(),
            env_overrides: Vec::new(),
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(self_pid) = self.pid
            && self_pid == pid
        {
            self.state = state;
            return true;
        }

        if let Some(ref mut next) = self.next {
            return next.set_state_pid(pid, state);
        }
        false
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                self.next = Some(Box::new(process));
            }
        }
    }

    pub async fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        let exit = self.handler.execute(ctx, self.argv.to_vec(), shell).await;
        self.finish(exit);
        Ok(())
    }

    fn finish(&mut self, exit: ExitStatus) {
        match exit {
            ExitStatus::ExitedWith(code) => {
                if code >= 0 {
                    self.state = ProcessState::Completed(code.clamp(0, 255) as u8, None);
                } else {
                    self.state = ProcessState::Completed(1, None);
                }
                debug!("Builtin process {} exited with code: {}", self.name, code);
            }
            ExitStatus::Running(_pid) => {
                self.state = ProcessState::Running;
                debug!("Builtin process {} is running", self.name);
            }
            ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => {
                self.state = ProcessState::Completed(0, None);
                debug!(
                    "Builtin process {} completed with control flow: {:?}",
                    self.name, exit
                );
            }
        }
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        // Background re-exec builtins own a real child pid and share the
        // external lifecycle semantics: only an actually-observed `waitpid`
        // status may enter the canonical tree. A foreground in-process
        // builtin carries `pid == getpid()`, which is never a child and must
        // never be passed to `waitpid`.
        //
        // A `Completed` self must not stop pipeline traversal: later stages
        // may still be `Running` and need polling.
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
                    debug!(
                        "builtin update_state: waitpid for pid {} failed: {}; keeping {:?}",
                        pid, err, self.state
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::process::Process;
    use nix::unistd::getpid;
    use std::time::{Duration, Instant};

    fn test_context() -> Context {
        Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true)
    }

    fn test_shell() -> Shell {
        Shell::new(Environment::new())
    }

    fn builtin_exit_zero(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(0)
    }

    fn builtin_exit_seven(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(7)
    }

    fn builtin_exit_negative(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(-1)
    }

    fn builtin_exit_async<'a>(
        _ctx: &'a Context,
        _argv: Vec<String>,
        _proxy: &'a mut dyn dsh_builtin::ShellProxy,
    ) -> dsh_builtin::BuiltinFuture<'a> {
        Box::pin(async { ExitStatus::ExitedWith(9) })
    }

    fn launch_state(
        cmd_fn: fn(&Context, Vec<String>, &mut dyn dsh_builtin::ShellProxy) -> ExitStatus,
    ) -> ProcessState {
        let mut process = BuiltinProcess::new(
            "test-builtin".to_string(),
            cmd_fn,
            vec!["test-builtin".into()],
        );
        let mut ctx = test_context();
        let mut shell = test_shell();

        futures::executor::block_on(process.launch(&mut ctx, &mut shell))
            .expect("builtin launch should succeed");

        process.state
    }

    #[test]
    fn launch_preserves_success_exit_code() {
        assert_eq!(
            launch_state(builtin_exit_zero),
            ProcessState::Completed(0, None)
        );
    }

    #[test]
    fn launch_preserves_nonzero_exit_code() {
        assert_eq!(
            launch_state(builtin_exit_seven),
            ProcessState::Completed(7, None)
        );
    }

    #[test]
    fn launch_maps_negative_exit_code_to_failure() {
        assert_eq!(
            launch_state(builtin_exit_negative),
            ProcessState::Completed(1, None)
        );
    }

    #[test]
    fn launch_awaits_async_builtin_handler() {
        let mut process = BuiltinProcess::new_handler(
            "test-async-builtin".to_string(),
            BuiltinHandler::Async {
                run: builtin_exit_async,
                fallback: builtin_exit_zero,
            },
            vec!["test-async-builtin".into()],
        );
        let mut ctx = test_context();
        let mut shell = test_shell();

        futures::executor::block_on(process.launch(&mut ctx, &mut shell)).unwrap();

        assert_eq!(process.state, ProcessState::Completed(9, None));
    }

    /// Poll `update_state` until the node leaves `Running`.
    ///
    /// Real children exit on their own schedule; the non-blocking poll keeps
    /// the test fast without inventing status. The caller must `mem::forget`
    /// the `std::process::Child` handle: `Child::wait` would consume the
    /// status first and `update_state` would only ever see `ECHILD`.
    fn poll_builtin_until_settled(process: &mut BuiltinProcess) -> ProcessState {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = process
                .update_state()
                .expect("builtin update_state always returns the node state");
            if !matches!(state, ProcessState::Running) {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for builtin child completion"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn builtin_with_child(argv0: &str, shell_cmd: &str) -> (BuiltinProcess, std::process::Child) {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(shell_cmd)
            .spawn()
            .expect("spawn test child");
        let pid = Pid::from_raw(child.id() as i32);
        let mut process = BuiltinProcess::new(
            argv0.to_string(),
            builtin_exit_zero,
            vec![argv0.to_string()],
        );
        process.pid = Some(pid);
        assert_eq!(process.state, ProcessState::Running);
        (process, child)
    }

    /// A background re-exec helper is a real child: its exit must flow from
    /// `waitpid` observation into the canonical `BuiltinProcess` state.
    #[test]
    fn reexec_like_builtin_child_is_reaped() {
        let (mut process, child) = builtin_with_child("dirs", "exit 0");
        // The status belongs to `update_state`, not to `Child::wait`.
        std::mem::forget(child);
        assert_eq!(
            poll_builtin_until_settled(&mut process),
            ProcessState::Completed(0, None)
        );
    }

    /// Non-zero helper exits are data and must be preserved verbatim.
    #[test]
    fn reexec_like_builtin_preserves_nonzero_exit() {
        let (mut process, child) = builtin_with_child("dirs", "exit 7");
        std::mem::forget(child);
        assert_eq!(
            poll_builtin_until_settled(&mut process),
            ProcessState::Completed(7, None)
        );
    }

    /// A foreground in-process builtin carries `pid == getpid()`: never a
    /// child, never passed to `waitpid`. `ECHILD` semantics keep it `Running`.
    #[test]
    fn builtin_update_state_does_not_wait_on_shell_pid() {
        let mut process = BuiltinProcess::new(
            "fg-builtin".to_string(),
            builtin_exit_zero,
            vec!["fg-builtin".to_string()],
        );
        process.pid = Some(getpid());
        process.state = ProcessState::Running;
        let state = process
            .update_state()
            .expect("builtin update_state always returns the node state");
        assert_eq!(state, ProcessState::Running);
        assert_eq!(process.state, ProcessState::Running);
    }

    /// `Builtin Completed` head must not block polling of an external tail:
    /// lifecycle semantics are variant-independent.
    #[test]
    fn completed_builtin_head_does_not_block_external_tail_polling() {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn tail child");
        let tail_pid = Pid::from_raw(child.id() as i32);
        std::mem::forget(child);

        let mut head = BuiltinProcess::new(
            "dirs".to_string(),
            builtin_exit_zero,
            vec!["dirs".to_string()],
        );
        head.state = ProcessState::Completed(0, None);
        let mut tail = Process::new("cat".to_string(), vec!["cat".to_string()]);
        tail.pid = Some(tail_pid);
        head.next = Some(Box::new(JobProcess::Command(tail)));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            head.update_state();
            let tail_state = head.next.as_deref().expect("pipeline tail").get_state();
            if matches!(tail_state, ProcessState::Completed(0, None)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "tail behind a Completed builtin head was never polled: {tail_state:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(head.state, ProcessState::Completed(0, None));
    }

    #[test]
    fn builtin_placement_single_foreground_runs_in_parent() {
        assert_eq!(
            builtin_execution_placement(true, false),
            BuiltinExecutionPlacement::Parent
        );
    }

    #[test]
    fn builtin_placement_pipeline_and_background_reexec() {
        use BuiltinExecutionPlacement::Reexec;
        // First/middle/last pipeline members all isolate, foreground or not.
        assert_eq!(builtin_execution_placement(true, true), Reexec);
        assert_eq!(builtin_execution_placement(false, false), Reexec);
        assert_eq!(builtin_execution_placement(false, true), Reexec);
    }

    /// The sync fallback now only serves in-process callers that cannot
    /// await. Background execution re-execs into a fresh runtime and always
    /// runs the real async handler (`launch` above) — no `fork()` child ever
    /// calls `block_on` or the fallback.
    #[test]
    fn async_builtin_fallback_stays_synchronous_only() {
        let process = BuiltinProcess::new_handler(
            "test-async-builtin".to_string(),
            BuiltinHandler::Async {
                run: builtin_exit_async,
                fallback: builtin_exit_zero,
            },
            vec!["test-async-builtin".into()],
        );
        let ctx = test_context();
        let mut shell = test_shell();

        let exit = process
            .handler
            .execute_sync(&ctx, process.argv.clone(), &mut shell);

        assert_eq!(exit, ExitStatus::ExitedWith(0));
    }
}
