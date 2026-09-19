//! Cached job lifecycle queries and transitions backed by the process tree.

use crate::process::{Job, JobProcess};

impl Job {
    /// Whether every stage in the canonical process tree has completed.
    pub(crate) fn is_process_tree_completed(&self) -> bool {
        self.process.as_deref().is_none_or(JobProcess::is_completed)
    }

    /// Whether the canonical process tree contains a stopped stage.
    pub(crate) fn has_stopped_process(&self) -> bool {
        self.process
            .as_deref()
            .is_some_and(JobProcess::has_stopped_process)
    }

    /// Reconcile the process tree and job summary after a successful resume.
    pub(crate) fn mark_stopped_processes_running(&mut self) {
        if let Some(process) = self.process.as_mut() {
            process.mark_stopped_processes_running();
        }
        self.refresh_lifecycle_state();
    }
}
