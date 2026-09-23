//! Pipeline exit-status policy: launch-time snapshot, separate from lifecycle.
//!
//! `Job.state` is the process-tree lifecycle truth (all completed → tail
//! stage state, fully stopped → observed stop, otherwise `Running`) and
//! never carries pipefail status. The logical pipeline status is a separate
//! policy evaluation owned by the `Job` at launch time:
//!
//! * pipefail OFF: the tail stage's `shell_exit_code()` (historical behavior).
//! * pipefail ON: the rightmost non-zero stage `shell_exit_code()`, or 0
//!   when every stage succeeded.
//!
//! The policy is snapshotted from `ShellOptions` once in `Job::launch`,
//! before any stage spawns. Finalization and `wait` must use
//! `Job::final_exit_status()` and never read live `Environment.shell_options`:
//! a pipeline started under one option value keeps that semantics even if
//! the parent shell flips the option while it runs. Signal deaths use the
//! existing `ProcessState::shell_exit_code()` (`128+N`) convention; no new
//! signal arithmetic lives here. An incomplete tree yields `None` rather
//! than a fabricated status.

use crate::process::Job;
use dsh_types::shell_options::{ShellOption, ShellOptions};

/// Frozen pipeline status policy for one launched `Job`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PipelineStatusPolicy {
    pipefail: bool,
}

impl PipelineStatusPolicy {
    pub(crate) fn from_shell_options(options: ShellOptions) -> Self {
        Self {
            pipefail: options.enabled(ShellOption::Pipefail),
        }
    }

    pub(crate) fn pipefail(&self) -> bool {
        self.pipefail
    }

    /// Resolve the logical pipeline status for `job`.
    ///
    /// Borrow-traverses the canonical tree so `NoCommand` stages participate
    /// exactly like external or builtin stages.
    pub(crate) fn resolve(&self, job: &Job) -> Option<i32> {
        let first = job.process.as_deref()?;
        if !self.pipefail() {
            let mut current = first;
            while let Some(next) = current.next_process() {
                current = next;
            }
            return current.get_state().shell_exit_code();
        }
        let mut current = Some(first);
        let mut rightmost_nonzero: Option<i32> = None;
        while let Some(process) = current {
            let code = process.get_state().shell_exit_code()?;
            if code != 0 {
                rightmost_nonzero = Some(code);
            }
            current = process.next_process();
        }
        Some(rightmost_nonzero.unwrap_or(0))
    }
}

impl Job {
    /// Canonical logical pipeline status for this job.
    ///
    /// Uses the launch-time [`PipelineStatusPolicy`] snapshot, never live
    /// shell options. `None` means the tree has not completed (or is
    /// missing): callers must not invent a status.
    pub(crate) fn final_exit_status(&self) -> Option<i32> {
        self.pipeline_status_policy.resolve(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{JobProcess, NoCommandProcess, Process, ProcessState};
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;

    fn job_with_codes(codes: &[ProcessState], pipefail: bool) -> Job {
        let mut job = Job::new("test".to_string(), Pid::from_raw(1));
        for (index, state) in codes.iter().enumerate() {
            let mut process = Process::new(format!("stage-{index}"), vec![]);
            process.state = *state;
            job.set_process(JobProcess::Command(process));
        }
        job.pipeline_status_policy = PipelineStatusPolicy { pipefail };
        job
    }

    fn completed(code: u8) -> ProcessState {
        ProcessState::Completed(code, None)
    }

    #[test]
    fn pipefail_off_uses_tail_status() {
        assert_eq!(
            job_with_codes(&[completed(1), completed(0)], false).final_exit_status(),
            Some(0)
        );
        assert_eq!(
            job_with_codes(&[completed(0), completed(7)], false).final_exit_status(),
            Some(7)
        );
    }

    #[test]
    fn pipefail_on_reports_rightmost_nonzero() {
        assert_eq!(
            job_with_codes(&[completed(0), completed(0)], true).final_exit_status(),
            Some(0)
        );
        assert_eq!(
            job_with_codes(&[completed(1), completed(0)], true).final_exit_status(),
            Some(1)
        );
        assert_eq!(
            job_with_codes(&[completed(7), completed(3), completed(0)], true).final_exit_status(),
            Some(3)
        );
        assert_eq!(
            job_with_codes(&[completed(7), completed(0), completed(3)], true).final_exit_status(),
            Some(3)
        );
        assert_eq!(
            job_with_codes(&[completed(0), completed(7), completed(0)], true).final_exit_status(),
            Some(7)
        );
    }

    #[test]
    fn pipefail_on_uses_existing_signal_convention() {
        let job = job_with_codes(
            &[ProcessState::signaled(Signal::SIGTERM), completed(0)],
            true,
        );
        assert_eq!(job.final_exit_status(), Some(143));
    }

    #[test]
    fn pipefail_on_returns_none_for_incomplete_tree() {
        let job = job_with_codes(&[completed(1), ProcessState::Running], true);
        assert_eq!(job.final_exit_status(), None);
    }

    #[test]
    fn pipefail_off_returns_none_for_incomplete_tail() {
        let job = job_with_codes(&[completed(1), ProcessState::Running], false);
        assert_eq!(job.final_exit_status(), None);
    }

    #[test]
    fn no_command_stage_participates_in_status() {
        let mut job = Job::new("test".to_string(), Pid::from_raw(1));
        let mut head = Process::new("head".to_string(), vec![]);
        head.state = completed(0);
        job.set_process(JobProcess::Command(head));
        let mut middle = NoCommandProcess::new(vec![], vec![], Some(5));
        middle.state = completed(5);
        job.set_process(JobProcess::NoCommand(middle));
        let mut tail = Process::new("tail".to_string(), vec![]);
        tail.state = completed(0);
        job.set_process(JobProcess::Command(tail));

        job.pipeline_status_policy = PipelineStatusPolicy { pipefail: false };
        assert_eq!(job.final_exit_status(), Some(0));
        job.pipeline_status_policy = PipelineStatusPolicy { pipefail: true };
        assert_eq!(job.final_exit_status(), Some(5));
    }

    #[test]
    fn missing_tree_has_no_status() {
        let job = Job::new("test".to_string(), Pid::from_raw(1));
        assert_eq!(job.final_exit_status(), None);
    }
}
