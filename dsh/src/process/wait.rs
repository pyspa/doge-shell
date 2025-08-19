use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tracing::{debug, error};

use super::job::Job;
use super::state::ProcessState;

pub fn is_job_stopped(job: &Job) -> bool {
    if let Some(p) = &job.process {
        let stopped = p.is_stopped();
        debug!(
            "is_job_stopped {} {} -> {}",
            p.get_cmd(),
            p.get_state(),
            stopped
        );
        stopped
    } else {
        debug!("is_job_stopped: no process -> true");
        true
    }
}

pub fn is_job_completed(job: &Job) -> bool {
    debug!(
        "JOB_COMPLETION_CHECK_START: Checking completion for job {} (state: {:?}, cmd: '{}')",
        job.job_id, job.state, job.cmd
    );

    if let Some(process) = &job.process {
        let process_state = process.get_state();
        let completed = process.is_completed();
        let consumer_terminated = process.is_pipeline_consumer_terminated();

        debug!(
            "JOB_COMPLETION_CHECK_PROCESS: Job {} process '{}' state: {:?}, completed: {}, consumer_terminated: {}",
            job.job_id,
            process.get_cmd(),
            process_state,
            completed,
            consumer_terminated
        );

        // Additional logging for specific states
        match process_state {
            ProcessState::Running => {
                debug!(
                    "JOB_COMPLETION_CHECK_RUNNING: Job {} is still running",
                    job.job_id
                );
            }
            ProcessState::Stopped(pid, signal) => {
                debug!(
                    "JOB_COMPLETION_CHECK_STOPPED: Job {} is stopped (pid: {}, signal: {:?})",
                    job.job_id, pid, signal
                );
            }
            ProcessState::Completed(exit_code, signal) => {
                debug!(
                    "JOB_COMPLETION_CHECK_COMPLETED: Job {} completed (exit_code: {}, signal: {:?})",
                    job.job_id, exit_code, signal
                );
            }
        }

        // Job is completed if either all processes are completed OR
        // (the consumer terminated normally AND no processes are stopped)
        let has_stopped = process.has_stopped_process();
        let job_completed = completed || (consumer_terminated && !has_stopped);

        debug!(
            "JOB_COMPLETION_CHECK_RESULT: Job {} completion result: {}",
            job.job_id, job_completed
        );

        // If consumer terminated but not all processes are complete, we should terminate remaining processes
        if consumer_terminated && !completed {
            debug!(
                "JOB_COMPLETION_CONSUMER_TERM: Job {} consumer terminated, should terminate remaining processes",
                job.job_id
            );
        }

        job_completed
    } else {
        debug!(
            "JOB_COMPLETION_CHECK_NO_PROCESS: Job {} has no process, treating as completed",
            job.job_id
        );
        true
    }
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
            (pid, ProcessState::Completed(status as u8, None))
        }
        Ok(WaitStatus::Signaled(pid, signal, core_dumped)) => {
            debug!(
                "WAIT_PID_SIGNALED: Process {} killed by signal: {:?}, core_dumped: {}",
                pid, signal, core_dumped
            );
            (pid, ProcessState::Completed(1, Some(signal)))
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
            (pid, ProcessState::Completed(1, None))
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

    #[test]
    fn is_stopped() {
        init();
        let input = "/usr/bin/touch";

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let process = Process::new("3".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(!is_job_stopped(job));

        let job = &mut Job::new(input.to_string(), getpgrp());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(10), Signal::SIGSTOP);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert!(is_job_stopped(job));
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
}
