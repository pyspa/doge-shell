//! Cached job lifecycle queries and transitions backed by the process tree.

use crate::process::{Job, JobProcess, state::ProcessState};
use tracing::debug;

impl Job {
    /// The most recently reconciled lifecycle summary of the process tree.
    ///
    /// This is a read-only snapshot, not a wait or a logical pipeline exit
    /// status. Use [`Self::final_exit_status`] for completion and pipefail.
    pub fn lifecycle_state(&self) -> ProcessState {
        self.state
    }

    /// Record a stage at the launch boundary, before wait reconciliation.
    pub(in crate::process) fn record_launched_process(&mut self, process: JobProcess) {
        self.state = process.get_state();
        self.set_process(process);
    }

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

impl Job {
    pub fn update_status(&mut self) -> bool {
        self.drain_exec_reports();
        let old_state = self.state;

        // Poll individual process states, then re-derive the job summary
        // from the whole tree. The root stage's returned state must not be
        // copied directly: it is order-dependent for pipelines.
        if let Some(process) = self.process.as_mut() {
            let _ = process.update_state();
        }
        self.refresh_lifecycle_state();
        self.drain_exec_reports();
        if let Err(error) = self.progress_launch_admission() {
            debug!("launch admission progress failed: {error}");
        }

        if old_state != self.state {
            debug!(
                "JOB_STATE_CHANGE: Job {} state changed: {:?} -> {:?} (pid: {:?}, pgid: {:?})",
                self.job_id, old_state, self.state, self.pid, self.pgid
            );

            match (&old_state, &self.state) {
                (ProcessState::Running, ProcessState::Stopped(pid, signal)) => {
                    debug!(
                        "JOB_STOPPED: Job {} stopped by signal {:?} (pid: {:?})",
                        self.job_id, signal, pid
                    );
                }
                (ProcessState::Stopped(_, _), ProcessState::Running) => {
                    debug!(
                        "JOB_RESUMED: Job {} resumed from stopped state",
                        self.job_id
                    );
                }
                (ProcessState::Running, ProcessState::Completed(exit_code, signal)) => {
                    debug!(
                        "JOB_COMPLETED: Job {} completed with exit_code: {}, signal: {:?}",
                        self.job_id, exit_code, signal
                    );
                }
                (ProcessState::Stopped(_, _), ProcessState::Completed(exit_code, signal)) => {
                    debug!(
                        "JOB_COMPLETED_FROM_STOP: Job {} completed from stopped state with exit_code: {}, signal: {:?}",
                        self.job_id, exit_code, signal
                    );
                }
                _ => {
                    debug!(
                        "JOB_STATE_OTHER: Job {} other state transition: {:?} -> {:?}",
                        self.job_id, old_state, self.state
                    );
                }
            }
        }

        let is_completed = self.is_process_tree_completed();
        debug!(
            "JOB_COMPLETION_CHECK: Job {} completion check result: {} (current state: {:?})",
            self.job_id, is_completed, self.state
        );

        is_completed
    }
}
