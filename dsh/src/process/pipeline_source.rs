//! Synthetic pipeline source: Smart Pipe previous output as bytes.
//!
//! A Smart Pipe head is not a command, builtin, or session object. It is a
//! finite byte source materialized from `OutputHistory::last_stdout()` with
//! the historical trailing-newline compatibility, then executed as a real
//! re-exec helper child that writes those bytes to its stdout (the pipeline
//! write fd). No feeder thread, no parent-side synchronous write, no hidden
//! builtin printing to the process stdout.

use super::job_process::JobProcess;
use super::state::ProcessState;
use super::wait::{WaitPidObservation, wait_pid_job};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::{Pid, getpid};
use std::os::unix::io::RawFd;

/// One synthetic source stage: owned bytes plus canonical pipeline links.
#[derive(Clone, PartialEq, Eq)]
pub struct PipelineSourceProcess {
    pub(crate) data: String,
    pub(crate) state: ProcessState,
    pub pid: Option<Pid>,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl std::fmt::Debug for PipelineSourceProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineSourceProcess")
            .field("data_len", &self.data.len())
            .field("state", &self.state)
            .field("pid", &self.pid)
            .field("has_next", &self.next.is_some())
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl super::job_process::JobProcess {
    /// Whether this stage is a synthetic data source rather than a command.
    pub(crate) fn is_synthetic_source(&self) -> bool {
        matches!(self, super::job_process::JobProcess::SyntheticSource(_))
    }

    /// Number of stages from this node to the pipeline tail, inclusive.
    pub(crate) fn stage_count(&self) -> usize {
        let mut count = 0;
        let mut current = Some(self);
        while let Some(node) = current {
            count += 1;
            current = node.next_process();
        }
        count
    }
}

// `PipelineSourceProcess` intentionally has no redirect/env metadata:
// those properties belong only to real command/no-command stages.

/// Spawn the synthetic source helper for one pipeline stage.
pub(crate) fn spawn_synthetic_source(
    ctx: &mut dsh_types::Context,
    shell: &crate::shell::Shell,
    process: &mut PipelineSourceProcess,
) -> anyhow::Result<nix::unistd::Pid> {
    // `spawn_pipeline_source` copies into JSON once; no clone needed here.
    let child = super::reexec::spawn_pipeline_source(
        ctx,
        shell,
        process.stdin,
        process.stdout,
        process.stderr,
        &process.data,
    )?;
    process.pid = Some(child);
    Ok(child)
}

impl PipelineSourceProcess {
    pub fn new(data: String) -> Self {
        Self {
            data,
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
        // `waitpid` status enters the tree. A still-running source stays
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
                        "pipeline source update_state: waitpid for pid {} failed: {}; keeping {:?}",
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
