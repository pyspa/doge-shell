//! Single-pid child wait observations for the canonical process tree.
//!
//! `ECHILD` is an observation about wait ownership/waitability, not a process
//! exit status. The code path that actually consumes a child status owns
//! recording that status into the canonical `JobProcess` tree; a later
//! `ECHILD` observer must never invent an exit code from it.

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

/// What a single `waitpid(pid)` call observed.
///
/// This separates three domains the old `Option<(Pid, ProcessState)>` return
/// conflated: an actually-observed state change (`State`), a live child with
/// nothing to consume (`StillAlive`), and a pid this caller can no longer
/// wait (`NoChild`). OS-level wait failures travel as `Err` and are never
/// converted into a synthetic `ProcessState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitPidObservation {
    /// `waitpid` reported a real state change: exited, signaled, or stopped.
    State(Pid, ProcessState),
    /// The child exists but has no state change for this caller to consume
    /// right now (`StillAlive`, or `Continued` which carries no lifecycle
    /// transition this shell tracks).
    StillAlive,
    /// `waitpid` returned `ECHILD`: the pid is not currently waitable by
    /// this caller (not our child, or its status was already consumed
    /// elsewhere). This is never an exit status.
    NoChild,
}

/// Decode one `waitpid` call into a [`WaitPidObservation`].
///
/// Shared by the single-pid helper below and the known-pid set scan in
/// `job_wait`, so both layers agree on `ECHILD`/`EINTR`/unexpected-status
/// handling instead of drifting apart.
pub(crate) fn observe_pid(pid: Pid, flags: WaitPidFlag) -> nix::Result<WaitPidObservation> {
    match waitpid(pid, Some(flags)) {
        Ok(WaitStatus::Exited(waited_pid, status)) => {
            debug!(
                "WAIT_PID_EXITED: Process {} exited normally with status: {}",
                waited_pid, status
            );
            Ok(WaitPidObservation::State(
                waited_pid,
                ProcessState::exited(status as u8),
            ))
        }
        Ok(WaitStatus::Signaled(waited_pid, signal, core_dumped)) => {
            debug!(
                "WAIT_PID_SIGNALED: Process {} killed by signal: {:?}, core_dumped: {}",
                waited_pid, signal, core_dumped
            );
            Ok(WaitPidObservation::State(
                waited_pid,
                ProcessState::signaled(signal),
            ))
        }
        Ok(WaitStatus::Stopped(waited_pid, signal)) => {
            debug!(
                "WAIT_PID_STOPPED: Process {} stopped by signal: {:?}",
                waited_pid, signal
            );
            Ok(WaitPidObservation::State(
                waited_pid,
                ProcessState::Stopped(waited_pid, signal),
            ))
        }
        Ok(WaitStatus::StillAlive) => {
            debug!("WAIT_PID_ALIVE: Process {} still alive (WNOHANG)", pid);
            Ok(WaitPidObservation::StillAlive)
        }
        Ok(WaitStatus::Continued(waited_pid)) => {
            // No `WCONTINUED` lifecycle is tracked: without `WCONTINUED` in
            // the requested flags this is defensive only. Report no state
            // change rather than inventing a `Running` transition.
            debug!("WAIT_PID_CONTINUED: Process {} continued", waited_pid);
            Ok(WaitPidObservation::StillAlive)
        }
        Err(nix::errno::Errno::ECHILD) => {
            debug!(
                "WAIT_PID_ECHILD: pid {} is not waitable by this caller (ECHILD); not an exit status",
                pid
            );
            Ok(WaitPidObservation::NoChild)
        }
        Err(err) => Err(err),
        // Portable fallback for the Linux-only `PtraceEvent`/`PtraceSyscall`
        // variants (nix advises against exhaustive `WaitStatus` matches).
        // Unreachable on macOS, where those variants do not exist.
        #[allow(unreachable_patterns)]
        status => {
            error!(
                "WAIT_PID_UNEXPECTED: Unexpected waitpid status for pid {}: {:?}",
                pid, status
            );
            Ok(WaitPidObservation::StillAlive)
        }
    }
}

pub fn wait_pid_job(pid: Pid, no_hang: bool) -> nix::Result<WaitPidObservation> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    debug!(
        "WAIT_PID_START: Starting waitpid for pid: {}, no_hang: {}, options: {:?}",
        pid, no_hang, options
    );

    let observation = observe_pid(pid, options)?;
    debug!(
        "WAIT_PID_RESULT: Returning result for pid {}: observation={:?}",
        pid, observation
    );
    Ok(observation)
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

    /// ECHILD is a wait-ownership observation, never a synthetic exit
    /// status: our own pid is not our child, so `waitpid` reports `ECHILD`.
    #[test]
    fn wait_pid_job_reports_no_child_for_non_child_pid() {
        init();
        assert_eq!(
            wait_pid_job(getpid(), true),
            Ok(WaitPidObservation::NoChild)
        );
    }

    /// A status already consumed by another waiter (`Child::wait` here)
    /// surfaces as `NoChild`, not as an invented `Completed(1)`.
    #[test]
    fn wait_pid_job_reports_no_child_for_already_reaped_child() {
        init();
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn short-lived sh");
        let pid = Pid::from_raw(child.id() as i32);
        let status = child.wait().expect("wait consumes the status first");
        assert_eq!(status.code(), Some(7));

        assert_eq!(wait_pid_job(pid, true), Ok(WaitPidObservation::NoChild));
    }

    /// Normal exit statuses still flow through untouched.
    #[test]
    fn wait_pid_job_observes_normal_exit_status() {
        init();
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn exiting sh");
        let pid = Pid::from_raw(child.id() as i32);
        let observation = wait_pid_job(pid, false).expect("waitpid succeeds");
        // The observation above already reaped the child; `Child::wait`
        // would now see ECHILD, so just release the handle.
        std::mem::forget(child);
        assert_eq!(
            observation,
            WaitPidObservation::State(pid, ProcessState::Completed(7, None))
        );
    }

    /// A live child under `WNOHANG` is `StillAlive`, distinctly not `NoChild`.
    #[test]
    fn wait_pid_job_reports_still_alive_for_running_child() {
        init();
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("spawn sleeping sh");
        let pid = Pid::from_raw(child.id() as i32);
        assert_eq!(wait_pid_job(pid, true), Ok(WaitPidObservation::StillAlive));
        child.kill().ok();
        let _ = child.wait();
    }

    #[test]
    fn wait_pid_job_converts_sigterm_to_143() {
        init();
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("kill -TERM $$")
            .spawn()
            .expect("spawn self-signalling sh");
        let pid = Pid::from_raw(child.id() as i32);
        // Blocking wait: the child terminates itself with SIGTERM immediately.
        let waited = loop {
            match wait_pid_job(pid, false).expect("waitpid succeeds") {
                WaitPidObservation::State(waited_pid, state) => break (waited_pid, state),
                // Unreachable with a blocking wait on an owned child, kept
                // for exhaustiveness.
                WaitPidObservation::StillAlive => continue,
                WaitPidObservation::NoChild => {
                    panic!("owned child pid {pid} became unwaitable (ECHILD)")
                }
            }
        };
        std::mem::forget(child);
        assert_eq!(waited.0, pid);
        assert_eq!(
            waited.1,
            ProcessState::Completed(143, Some(Signal::SIGTERM))
        );
        assert_eq!(waited.1.shell_exit_code(), Some(143));
    }
}
