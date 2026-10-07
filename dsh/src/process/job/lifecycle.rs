//! Job lifecycle transitions and process-group ownership predicates.

use crate::process::{Job, JobProcess, state::ProcessState};
use nix::{errno::Errno, unistd::Pid};

impl Job {
    /// Whether every stage in the canonical process tree has completed.
    pub(crate) fn is_process_tree_completed(&self) -> bool {
        self.process.as_deref().is_none_or(JobProcess::is_completed)
    }

    /// Whether the canonical process tree contains a stopped stage.
    ///
    /// Used for SIGCONT resume decisions (`bg` / `fg`): any stopped stage
    /// needs `SIGCONT`, even when the job as a whole is still running.
    pub(crate) fn has_stopped_process(&self) -> bool {
        self.process
            .as_deref()
            .is_some_and(JobProcess::has_stopped_process)
    }

    /// Whether the whole job can be treated as stopped: at least one
    /// `Stopped` stage and no `Running` stage. Used for foreground-wait
    /// termination, `Job.state` summary derivation, and the Ctrl-Z
    /// resume-shortcut selection. A missing process tree is not stopped.
    pub(crate) fn is_fully_stopped(&self) -> bool {
        self.process
            .as_deref()
            .is_some_and(JobProcess::is_fully_stopped)
    }

    /// Sync the job-table summary from the process tree (real `Stopped`,
    /// never synthesized).
    ///
    /// Derivation order: all-completed → final completion state; fully
    /// stopped (at least one `Stopped`, no `Running`) → the actual observed
    /// stop state; anything else (including partial stops) → `Running`.
    pub(crate) fn refresh_lifecycle_state(&mut self) {
        if self.is_process_tree_completed() {
            self.state = self.last_process_state();
            return;
        }
        if self.is_fully_stopped()
            && let Some(process) = &self.process
            && let Some(stopped) = process.first_stopped_state()
        {
            self.state = stopped;
            return;
        }
        self.state = ProcessState::Running;
    }

    /// Reconcile the process tree and job summary after a successful resume.
    pub(crate) fn mark_stopped_processes_running(&mut self) {
        if let Some(process) = self.process.as_mut() {
            process.mark_stopped_processes_running();
        }
        self.refresh_lifecycle_state();
    }
}

/// A post-exec EACCES is harmless only when the child already belongs to
/// the positive job group approved by the existing signal ownership guard.
pub(super) fn parent_setpgid_exec_race(
    error: Errno,
    owned_group: Option<Pid>,
    observed_group: std::result::Result<Pid, Errno>,
) -> bool {
    error == Errno::EACCES
        && owned_group.is_some_and(|group| group.as_raw() > 0 && observed_group == Ok(group))
}
