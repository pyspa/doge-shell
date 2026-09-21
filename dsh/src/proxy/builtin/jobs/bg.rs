//! Background job selection and SIGCONT dispatch.

use super::{finalize_background_resume, parse_job_spec};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use nix::sys::signal::{Signal, killpg};
use tracing::{debug, error};

/// Execute the `bg` builtin command.
///
/// Resumes a stopped job in the background.
pub fn execute_bg(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    debug!(
        "BG_CMD_START: Starting bg command - wait_jobs.len(): {}, args: {:?}",
        shell.wait_jobs.len(),
        argv
    );

    if shell.wait_jobs.is_empty() {
        debug!("BG_CMD_NO_JOBS: No jobs available for bg command");
        ctx.write_stdout("bg: there are no suitable jobs")?;
    } else {
        let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
        debug!("BG_CMD_SPEC: Job specification: '{}'", job_spec);

        debug!("BG_CMD_AVAILABLE_JOBS: Current job list:");
        for (i, job) in shell.wait_jobs.iter().enumerate() {
            debug!(
                "BG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
                i, job.job_id, job.pid, job.state, job.foreground, job.cmd
            );
        }

        let job_index = if job_spec.is_empty() {
            debug!("BG_CMD_FIND_STOPPED: Looking for most recent stopped job");
            let found_index = shell
                .wait_jobs
                .iter()
                .enumerate()
                .rev()
                .find(|(_, job)| job.has_stopped_process())
                .map(|(index, job)| {
                    debug!(
                        "BG_CMD_FOUND_STOPPED: Found stopped job {} at index {}",
                        job.job_id, index
                    );
                    index
                });
            if found_index.is_none() {
                debug!("BG_CMD_NO_STOPPED: No stopped jobs found");
            }
            found_index
        } else {
            debug!("BG_CMD_PARSE_SPEC: Parsing job specification: '{job_spec}'");
            parse_job_spec(job_spec, &shell.wait_jobs)
        };

        if let Some(index) = job_index {
            let job = &shell.wait_jobs[index];
            debug!(
                "BG_CMD_SELECTED: Selected job {} at index {} for background",
                job.job_id, index
            );

            if !job.has_stopped_process() {
                let error_msg = format!("bg: job {} is already running", job.job_id);
                debug!("BG_CMD_ALREADY_RUNNING: {error_msg}");
                ctx.write_stderr(&error_msg)?;
                return Err(anyhow::anyhow!(error_msg));
            }

            let job = shell.wait_jobs.remove(index);
            debug!(
                "BG_CMD_JOB_DETAILS: Job details before bg - state: {:?}, pgid: {:?}, pid: {:?}",
                job.state, job.pgid, job.pid
            );

            let job_id = job.job_id;
            let job_cmd = job.cmd.clone();
            let had_pgid = job.pgid.is_some();
            let resume_result: Result<()> = if let Some(pgid) = job.pgid {
                debug!(
                    "BG_CMD_SIGCONT: Sending SIGCONT to process group {} for job {}",
                    pgid, job.job_id
                );
                killpg(pgid, Signal::SIGCONT).map_err(Into::into)
            } else {
                Err(anyhow::anyhow!(
                    "bg: job {} has no process group",
                    job.job_id
                ))
            };

            let result = super::block_on_job_control_future(finalize_background_resume(
                shell,
                job,
                resume_result,
            ))?;
            match result {
                Ok(()) => {
                    debug!(
                        "BG_CMD_SIGCONT_SUCCESS: SIGCONT sent successfully to job {}",
                        job_id
                    );
                    ctx.write_stdout(&format!("dsh: job {} '{}' to background", job_id, job_cmd))
                        .ok();
                    debug!("BG_CMD_SUCCESS: Job moved to background successfully");
                }
                Err(err) => {
                    error!("BG_CMD_SIGCONT_ERROR: Failed to resume job {job_id}: {err}");
                    let error_msg = if had_pgid {
                        format!("bg: failed to resume job: {err}")
                    } else {
                        err.to_string()
                    };
                    ctx.write_stderr(&error_msg).ok();
                    return Err(err);
                }
            }
        } else {
            let error_msg = if job_spec.is_empty() {
                "bg: no stopped jobs".to_string()
            } else {
                format!("bg: job not found: {job_spec}")
            };
            debug!("BG_CMD_NOT_FOUND: {error_msg}");
            ctx.write_stderr(&error_msg)?;
            return Err(anyhow::anyhow!(error_msg));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{finalize_background_resume, finalize_foreground_job};
    use crate::environment::Environment;
    use crate::process::wait::is_job_completed;
    use crate::process::{Job, JobProcess, Process, ProcessState};

    use crate::shell::Shell;
    use dsh_types::Context;
    use nix::sys::signal::Signal;
    use nix::unistd::{Pid, getpgrp, getpid};

    fn stopped_job(job_id: usize, signal: Signal) -> Job {
        let mut job = Job::new("sleep 60".to_string(), getpgrp());
        job.job_id = job_id;
        let pid = Pid::from_raw(424247 + job_id as i32);
        job.pid = Some(pid);
        job.pgid = None;
        let mut process = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
        process.pid = Some(pid);
        process.state = ProcessState::Stopped(pid, signal);
        job.set_process(JobProcess::Command(process));
        job.state = ProcessState::Running;
        job
    }

    fn stopped_producer_completed_consumer_job(job_id: usize) -> Job {
        let mut job = Job::new("producer | consumer".to_string(), getpgrp());
        job.job_id = job_id;

        let producer_pid = Pid::from_raw(424245);
        let mut producer = Process::new("producer".to_string(), vec!["producer".to_string()]);
        producer.pid = Some(producer_pid);
        producer.state = ProcessState::Stopped(producer_pid, Signal::SIGTSTP);
        job.set_process(JobProcess::Command(producer));

        let consumer_pid = Pid::from_raw(424246);
        let mut consumer = Process::new("consumer".to_string(), vec!["consumer".to_string()]);
        consumer.pid = Some(consumer_pid);
        consumer.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(consumer));

        job.pid = Some(producer_pid);
        job.pgid = Some(producer_pid);
        job.state = ProcessState::Stopped(producer_pid, Signal::SIGTSTP);
        job
    }

    #[tokio::test]
    async fn bg_success_requeues_resumed_producer_after_consumer_completed() {
        let mut shell = Shell::new(Environment::new());
        let job = stopped_producer_completed_consumer_job(18);

        finalize_background_resume(&mut shell, job, Ok(()))
            .await
            .expect("finalize");

        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert_eq!(requeued.job_id, 18);
        assert_eq!(requeued.state, ProcessState::Running);
        let producer = requeued.process.as_deref().expect("producer");
        assert_eq!(producer.get_state(), ProcessState::Running);
        assert_eq!(
            producer.next_process().map(JobProcess::get_state),
            Some(ProcessState::Completed(0, None))
        );
        // Strict completion: a running producer with a completed consumer
        // is not complete and must stay in the job table.
        assert!(!is_job_completed(requeued));
        assert!(!requeued.is_process_tree_completed());
    }

    #[test]
    fn background_job_retains_running_producer_after_consumer_completion() {
        use crate::shell::job::check_job_state;

        let mut shell = Shell::new(Environment::new());
        let mut job = stopped_producer_completed_consumer_job(19);
        // Simulate `bg` resume: stopped producer becomes running, consumer
        // stays completed.
        job.mark_stopped_processes_running();
        assert_eq!(
            job.process.as_deref().map(JobProcess::get_state),
            Some(ProcessState::Running)
        );
        // The fixture pids are not waitable by this caller (ECHILD), which
        // must preserve the tree verbatim: no synthetic Completed(1) may
        // rewrite the resumed Running producer below.
        shell.wait_jobs.push(job);

        let completed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime")
            .block_on(check_job_state(&mut shell))
            .expect("check job state");
        assert!(
            completed.is_empty(),
            "Running / Completed(0) must not be collected as completed"
        );
        assert_eq!(shell.wait_jobs.len(), 1);
        assert!(!shell.wait_jobs[0].is_process_tree_completed());
        assert!(!is_job_completed(&shell.wait_jobs[0]));
    }

    fn pipeline_job(job_id: usize, states: &[ProcessState]) -> Job {
        let mut job = Job::new("producer | consumer".to_string(), getpgrp());
        job.job_id = job_id;
        for (index, state) in states.iter().enumerate() {
            let pid = Pid::from_raw(425000 + job_id as i32 * 10 + index as i32);
            let mut proc = Process::new(format!("stage-{}", index + 1), vec![]);
            proc.pid = Some(pid);
            proc.state = *state;
            job.set_process(JobProcess::Command(proc));
            if index == 0 {
                job.pid = Some(pid);
                job.pgid = Some(pid);
            }
        }
        job.state = ProcessState::Running;
        job
    }

    #[tokio::test]
    async fn foreground_finalizer_requeues_running_producer_after_consumer_completion() {
        let mut shell = Shell::new(Environment::new());
        let job = pipeline_job(
            20,
            &[ProcessState::Running, ProcessState::Completed(0, None)],
        );

        finalize_foreground_job(&mut shell, job, Ok(()))
            .await
            .expect("finalize");

        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert!(!requeued.is_process_tree_completed());
        assert_eq!(requeued.state, ProcessState::Running);
    }

    #[tokio::test]
    async fn foreground_finalizer_requeues_stopped_producer_after_consumer_completion() {
        let mut shell = Shell::new(Environment::new());
        let stopped_pid = Pid::from_raw(425200);
        let job = pipeline_job(
            21,
            &[
                ProcessState::Stopped(stopped_pid, Signal::SIGTSTP),
                ProcessState::Completed(0, None),
            ],
        );

        finalize_foreground_job(&mut shell, job, Ok(()))
            .await
            .expect("finalize");

        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert!(!requeued.is_process_tree_completed());
        assert_eq!(
            requeued.state,
            ProcessState::Stopped(stopped_pid, Signal::SIGTSTP),
            "stopped producer must keep its real observed stop state"
        );
    }

    #[tokio::test]
    async fn foreground_finalizer_drops_all_completed_pipeline() {
        let mut shell = Shell::new(Environment::new());
        let job = pipeline_job(
            22,
            &[
                ProcessState::Completed(0, None),
                ProcessState::Completed(0, None),
            ],
        );

        finalize_foreground_job(&mut shell, job, Ok(()))
            .await
            .expect("finalize");

        assert!(
            shell.wait_jobs.is_empty(),
            "all-completed pipeline must be dropped"
        );
    }

    #[test]
    fn bg_default_selection_prefers_most_recent_stopped_process_tree() {
        let mut shell = Shell::new(Environment::new());
        shell.wait_jobs.push(stopped_job(16, Signal::SIGSTOP));
        shell.wait_jobs.push(stopped_job(17, Signal::SIGTSTP));
        let mut ctx = Context::new_safe(getpid(), getpgrp(), true);
        ctx.interactive = false;

        let result = super::execute_bg(&mut shell, &ctx, vec!["bg".to_string()]);

        let err = result.expect_err("most recent stopped tree should be selected");
        assert!(err.to_string().contains("job 17 has no process group"));
        assert_eq!(shell.wait_jobs.len(), 2);
        assert_eq!(shell.wait_jobs[0].job_id, 16);
        assert_eq!(shell.wait_jobs[1].job_id, 17);
        assert!(shell.wait_jobs.iter().all(Job::has_stopped_process));
    }
}
