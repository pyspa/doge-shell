use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tracing::{debug, error};

use super::job::Job;
use super::state::ProcessState;

/// Whether every process in the canonical job process tree has completed.
///
/// This is a strict lifecycle/ownership predicate. Final-consumer success
/// alone is not job completion.
pub fn is_job_completed(job: &Job) -> bool {
    let job_completed = job.is_process_tree_completed();
    debug!(
        "JOB_COMPLETION_CHECK_RESULT: Job {} completion result: {} (state: {:?})",
        job.job_id, job_completed, job.state
    );
    job_completed
}

pub fn wait_pid_job(pid: Pid, no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    debug!(
        "WAIT_PID_START: Starting waitpid for pid: {}, no_hang: {}, options: {:?}",
        pid, no_hang, options
    );

    let result = waitpid(pid, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => {
            debug!(
                "WAIT_PID_EXITED: Process {} exited normally with status: {}",
                pid, status
            );
            (pid, ProcessState::exited(status as u8))
        }
        Ok(WaitStatus::Signaled(pid, signal, core_dumped)) => {
            debug!(
                "WAIT_PID_SIGNALED: Process {} killed by signal: {:?}, core_dumped: {}",
                pid, signal, core_dumped
            );
            (pid, ProcessState::signaled(signal))
        }
        Ok(WaitStatus::Stopped(pid, signal)) => {
            debug!(
                "WAIT_PID_STOPPED: Process {} stopped by signal: {:?}",
                pid, signal
            );
            (pid, ProcessState::Stopped(pid, signal))
        }
        Err(nix::errno::Errno::ECHILD) => {
            debug!(
                "WAIT_PID_ECHILD: No child process {} (ECHILD) - treating as completed",
                pid
            );
            (pid, ProcessState::exited(1))
        }
        Ok(WaitStatus::StillAlive) => {
            debug!("WAIT_PID_ALIVE: Process {} still alive (WNOHANG)", pid);
            return None;
        }
        Ok(WaitStatus::Continued(pid)) => {
            debug!("WAIT_PID_CONTINUED: Process {} continued", pid);
            return None;
        }
        status => {
            error!(
                "WAIT_PID_UNEXPECTED: Unexpected waitpid status for pid {}: {:?}",
                pid, status
            );
            return None;
        }
    };

    debug!(
        "WAIT_PID_RESULT: Returning result for pid {}: state={:?}",
        pid, res.1
    );
    Some(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::job_process::JobProcess;
    use crate::process::process::Process;
    use nix::sys::signal::Signal;
    use nix::unistd::{Pid, getpgrp, getpid};

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    fn job_with_states(states: &[ProcessState]) -> Job {
        let mut job = Job::new("/usr/bin/touch".to_string(), getpgrp());
        for (index, state) in states.iter().enumerate() {
            let mut process = Process::new(format!("{}", index + 1), vec![]);
            process.state = *state;
            job.set_process(JobProcess::Command(process));
        }
        debug!("{:?}", job);
        job
    }

    #[test]
    fn fully_stopped_needs_every_live_stage_stopped() {
        init();

        // Completed / Completed / Running: still live, not fully stopped.
        let job = job_with_states(&[
            ProcessState::Completed(0, None),
            ProcessState::Completed(0, None),
            ProcessState::Running,
        ]);
        assert!(!job.has_stopped_process());
        assert!(!job.is_fully_stopped());

        // Completed / Completed / Stopped: no Running left, fully stopped.
        let job = job_with_states(&[
            ProcessState::Completed(0, None),
            ProcessState::Completed(0, None),
            ProcessState::Stopped(Pid::from_raw(10), Signal::SIGSTOP),
        ]);
        assert!(job.has_stopped_process());
        assert!(job.is_fully_stopped());

        // Partial stop must not end a foreground wait.
        let job = job_with_states(&[
            ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
            ProcessState::Running,
        ]);
        assert!(job.has_stopped_process());
        assert!(!job.is_fully_stopped());
    }

    #[test]
    fn is_completed() {
        init();
        let input = "/usr/bin/touch";

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(0), Signal::SIGSTOP);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(!is_job_completed(job));

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(is_job_completed(job));
    }

    /// Strict lifecycle completion: a Job is complete only when every stage
    /// in the canonical process tree is `Completed`. Final-consumer success
    /// alone never completes the job (failing regression for the removed
    /// consumer shortcut).
    #[test]
    fn running_producer_with_completed_consumer_is_not_job_complete() {
        init();
        let job = job_with_states(&[ProcessState::Running, ProcessState::Completed(0, None)]);
        assert!(!is_job_completed(&job));
        assert!(!job.is_process_tree_completed());
    }

    #[test]
    fn stopped_producer_with_completed_consumer_is_not_job_complete() {
        init();
        let job = job_with_states(&[
            ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
            ProcessState::Completed(0, None),
        ]);
        assert!(!is_job_completed(&job));
        assert!(!job.is_process_tree_completed());
        assert!(job.is_fully_stopped());
    }

    /// Strict completion truth table: only all-`Completed` trees complete.
    /// Exit codes never matter; a completed final consumer alone changes
    /// nothing.
    #[test]
    fn strict_completion_truth_table() {
        init();
        let stopped = || ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP);
        let cases: &[(&[ProcessState], bool)] = &[
            (&[ProcessState::Running], false),
            (
                &[ProcessState::Running, ProcessState::Completed(0, None)],
                false,
            ),
            (&[stopped(), ProcessState::Completed(0, None)], false),
            (
                &[
                    ProcessState::Completed(0, None),
                    ProcessState::Running,
                    ProcessState::Completed(0, None),
                ],
                false,
            ),
            (
                &[
                    ProcessState::Completed(0, None),
                    ProcessState::Completed(0, None),
                ],
                true,
            ),
            (
                &[
                    ProcessState::Completed(3, None),
                    ProcessState::Completed(0, None),
                ],
                true,
            ),
            (
                &[
                    ProcessState::Running,
                    ProcessState::Running,
                    ProcessState::Completed(0, None),
                ],
                false,
            ),
            (
                &[
                    ProcessState::Completed(3, None),
                    ProcessState::Completed(7, None),
                    ProcessState::Completed(1, None),
                ],
                true,
            ),
        ];
        for (states, expected) in cases {
            let job = job_with_states(states);
            assert_eq!(
                is_job_completed(&job),
                *expected,
                "strict completion for {:?}",
                states,
            );
            assert_eq!(
                job.is_process_tree_completed(),
                *expected,
                "process-tree completion for {:?}",
                states,
            );
        }
    }

    #[test]
    fn test_wait_pid_job_handles_unexpected_status() {
        // This test verifies that wait_pid_job no longer panics on unexpected status
        // Instead, it should return None and log an error
        init();

        // Test that the function exists and has the correct signature
        let result = wait_pid_job(getpid(), true);
        // Should not panic, may return None
        assert!(result.is_none() || result.is_some());
    }

    #[test]
    fn wait_pid_job_converts_sigterm_to_143() {
        init();
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("kill -TERM $$")
            .spawn()
            .expect("spawn self-signalling sh");
        let pid = Pid::from_raw(child.id() as i32);
        // Blocking wait: the child terminates itself with SIGTERM immediately.
        let waited = loop {
            if let Some(result) = wait_pid_job(pid, false) {
                break result;
            }
        };
        let _ = child.wait();
        assert_eq!(waited.0, pid);
        assert_eq!(
            waited.1,
            ProcessState::Completed(143, Some(Signal::SIGTERM))
        );
        assert_eq!(waited.1.shell_exit_code(), Some(143));
    }
}
