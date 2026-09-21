//! One no-command pipeline stage as a real re-exec helper child.
//!
//! A runtime-expanded pipeline stage with no command name is still a real
//! pipeline stage: it must never be dropped or rewired (`A | empty | C`
//! must not become `A | C`). Single-stage no-command execution stays in the
//! current shell ([`crate::shell::no_command::execute_no_command`]); a
//! no-command member of a multi-stage pipeline executes here, in an
//! isolated re-exec helper, so assignments cannot mutate the parent shell.
//!
//! Redirections are applied exactly once by the normal parent pipeline
//! wiring before the helper spawns (see [`super::job_process`] launch
//! flow); the helper receives final stdio and never reapplies them.

use super::job_process::JobProcess;
use super::redirect::Redirect;
use super::state::ProcessState;
use super::wait::{WaitPidObservation, wait_pid_job};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::{Pid, getpid};
use std::os::unix::io::RawFd;

/// One no-command pipeline stage: owned assignments plus canonical
/// pipeline links. The helper child applies the assignments in a fresh
/// shell snapshot and exits with the last command-substitution status
/// (or 0 when there was none).
#[derive(Clone, PartialEq, Eq)]
pub struct NoCommandProcess {
    pub(crate) assignments: Vec<(String, String)>,
    pub(crate) redirects: Vec<Redirect>,
    pub(crate) last_command_substitution_status: Option<i32>,
    pub(crate) state: ProcessState,
    pub pid: Option<Pid>,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl std::fmt::Debug for NoCommandProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Assignment values may carry secrets: report counts only.
        f.debug_struct("NoCommandProcess")
            .field("assignments_len", &self.assignments.len())
            .field("redirects_len", &self.redirects.len())
            .field(
                "has_last_command_substitution_status",
                &self.last_command_substitution_status.is_some(),
            )
            .field("state", &self.state)
            .field("pid", &self.pid)
            .field("has_next", &self.next.is_some())
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

/// Spawn the no-command helper for one pipeline stage.
pub(crate) fn spawn_no_command_process(
    ctx: &mut dsh_types::Context,
    shell: &crate::shell::Shell,
    process: &mut NoCommandProcess,
) -> anyhow::Result<nix::unistd::Pid> {
    let child = super::reexec::spawn_no_command(
        ctx,
        shell,
        process.stdin,
        process.stdout,
        process.stderr,
        &process.assignments,
        process.last_command_substitution_status,
    )?;
    process.pid = Some(child);
    Ok(child)
}

impl NoCommandProcess {
    pub fn new(
        assignments: Vec<(String, String)>,
        redirects: Vec<Redirect>,
        last_command_substitution_status: Option<i32>,
    ) -> Self {
        Self {
            assignments,
            redirects,
            last_command_substitution_status,
            state: ProcessState::Running,
            pid: None,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
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
            Some(ref mut p) => p.link(process),
            None => self.next = Some(Box::new(process)),
        }
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        // Same lifecycle as external/re-exec children: only an observed
        // `waitpid` status enters the tree. A still-running stage stays
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
                        "no-command update_state: waitpid for pid {} failed: {}; keeping {:?}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Process;

    #[test]
    fn lifecycle_pid_state_and_next_traversal() {
        let stage = NoCommandProcess::new(
            vec![("FOO".to_string(), "bar".to_string())],
            vec![],
            Some(7),
        );
        assert_eq!(stage.pid, None);
        assert!(matches!(stage.state, ProcessState::Running));
        // Debug reports counts, never secret-carrying values.
        let debug = format!("{stage:?}");
        assert!(debug.contains("assignments_len"));
        assert!(!debug.contains("bar"));

        let mut node = JobProcess::NoCommand(stage);
        assert_eq!(node.get_pid(), None);
        let pid = Pid::from_raw(424242);
        node.set_pid(Some(pid));
        assert_eq!(node.get_pid(), Some(pid));
        assert_eq!(node.owned_child_pid(Pid::from_raw(1)), Some(pid));
        // The shell's own pid is never a managed child.
        assert_eq!(node.owned_child_pid(pid), None);

        // waitpid-observed state enters through the pid-routed setter.
        assert!(node.set_state_pid(pid, ProcessState::Completed(7, None)));
        assert!(matches!(node.get_state(), ProcessState::Completed(7, _)));
        assert!(node.is_completed());
        assert!(!node.is_fully_stopped());

        // Unknown pids match nothing.
        assert!(!node.set_state_pid(Pid::from_raw(999999), ProcessState::Running));

        // Link traversal reaches the tail.
        node.link(JobProcess::Command(Process::new(
            "cat".to_string(),
            vec!["cat".to_string()],
        )));
        let tail = node.next_process().expect("tail");
        assert_eq!(tail.get_cmd(), "cat");
        let tail_pid = Pid::from_raw(424243);
        node.next_process_mut()
            .expect("tail")
            .set_pid(Some(tail_pid));
        assert!(node.set_state_pid(tail_pid, ProcessState::Running));
        assert!(matches!(
            node.next_process().expect("tail").get_state(),
            ProcessState::Running
        ));
    }
}
