//! Background job selection and SIGCONT dispatch.
//!
//! `execute_bg` is the thin sync bridge over [`background_jobs`]; explicit
//! operands are all resolved against one reconciled, pre-mutation
//! active-table snapshot into stable job IDs, then resumed best-effort.
//! Each selected job transfers through [`finalize_background_resume`], the
//! single owner of the stopped→running transition and the canonical
//! completed-job finalizer.

use super::{finalize_background_resume, parse_job_spec};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tracing::{debug, error};

/// Execute the `bg` builtin command.
///
/// Resumes stopped jobs in the background. Thin sync bridge over
/// [`background_jobs`]: this boundary only blocks the calling worker thread
/// until the async driver completes.
///
/// Errors are prefixless; the builtin wrapper owns the final `bg: ` prefix
/// and core failure paths never write to stderr. Successful resumes still
/// notify on stdout (`dsh: job N '...' to background`).
pub fn execute_bg(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    debug!(
        "BG_CMD_START: Starting bg command - wait_jobs.len(): {}, args: {:?}",
        shell.wait_jobs.len(),
        argv
    );
    super::block_on_job_control_future(background_jobs(shell, ctx, argv))??;
    Ok(())
}

/// A single `bg` operand resolved against the pre-mutation active table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BgOperandResolution {
    /// Operand plus the stable job ID it selected.
    Target { operand: String, job_id: usize },
    /// Operand that selected nothing; recorded, never fail-fast.
    Invalid { operand: String, reason: String },
}

/// One failed `bg` target, kept in operand order for the aggregate error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BgFailure {
    pub(crate) operand: String,
    pub(crate) reason: String,
}

/// A successful background resume, for the stdout notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BgResumeSuccess {
    pub(crate) job_id: usize,
    pub(crate) cmd: String,
}

/// Split `bg` argv (including `argv[0]`) into jobspec operands.
///
/// `bg` owns no options yet, but `--` separates operands so
/// `bg -- %1 %2` works. `-`/`+` are previous/current job aliases, never
/// options; any other `-`-prefixed token is a usage error. An empty operand
/// (`bg ""`) keeps the legacy `fg` behavior and resolves to the current job
/// downstream via [`parse_job_spec`], rather than becoming a new error case.
///
/// Errors are prefixless; the builtin wrapper owns the final `bg: ` prefix.
pub(crate) fn parse_bg_operands(argv: &[String]) -> Result<Vec<String>> {
    let mut operands = Vec::new();
    let mut end_of_options = false;
    for arg in argv.iter().skip(1) {
        if end_of_options {
            operands.push(arg.clone());
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            continue;
        }
        if arg.starts_with('-') && arg != "-" {
            return Err(anyhow::anyhow!("unsupported option: {arg}"));
        }
        operands.push(arg.clone());
    }
    Ok(operands)
}

/// Resolve every operand against the reconciled active table before any
/// mutation, mapping each to its stable job ID.
///
/// Index-based re-resolution after a remove/requeue would drift (`bg %- %+`
/// could select the requeued first job twice), so the driver never carries
/// `wait_jobs` indices across mutations — only these stable IDs.
pub(crate) fn resolve_bg_targets(
    operands: &[String],
    wait_jobs: &[crate::process::Job],
) -> Vec<BgOperandResolution> {
    operands
        .iter()
        .map(|operand| match parse_job_spec(operand, wait_jobs) {
            Some(index) => BgOperandResolution::Target {
                operand: operand.clone(),
                job_id: wait_jobs[index].job_id,
            },
            None => BgOperandResolution::Invalid {
                operand: operand.clone(),
                reason: "job not found".to_string(),
            },
        })
        .collect()
}

/// Select the default `bg` target: the most recent job whose canonical
/// process tree holds a stopped process. The stale `job.state` summary
/// never decides; [`crate::process::Job::has_stopped_process`] is authority.
pub(crate) fn default_bg_target(wait_jobs: &[crate::process::Job]) -> Option<usize> {
    wait_jobs
        .iter()
        .rev()
        .find(|job| job.has_stopped_process())
        .map(|job| job.job_id)
}

/// Resume one job by stable ID, transferring ownership through
/// [`finalize_background_resume`] on every path.
///
/// The job is only removed after its process tree is confirmed stopped; a
/// missing job, an already-running tree, a missing process group, or a
/// SIGCONT failure all keep active ownership (requeued through the
/// finalizer) and report a prefixless error.
///
/// `send_cont` injects the SIGCONT delivery so orchestration tests never
/// send real signals; production passes `killpg(pgid, SIGCONT)`.
pub(crate) async fn resume_background_job_with<F>(
    shell: &mut Shell,
    job_id: usize,
    send_cont: &mut F,
) -> Result<BgResumeSuccess>
where
    F: FnMut(Pid) -> Result<()>,
{
    let Some(index) = shell.wait_jobs.iter().position(|job| job.job_id == job_id) else {
        return Err(anyhow::anyhow!("job {job_id} is no longer active"));
    };
    if !shell.wait_jobs[index].has_stopped_process() {
        return Err(anyhow::anyhow!("job {job_id} is already running"));
    }

    let job = shell.wait_jobs.remove(index);
    debug!(
        "BG_CMD_JOB_DETAILS: Job details before bg - state: {:?}, pgid: {:?}, pid: {:?}",
        job.state, job.pgid, job.pid
    );

    let job_id = job.job_id;
    let job_cmd = job.cmd.clone();
    let resume_result: Result<()> = match job.pgid {
        Some(pgid) => {
            debug!(
                "BG_CMD_SIGCONT: Sending SIGCONT to process group {} for job {}",
                pgid, job.job_id
            );
            send_cont(pgid)
        }
        None => Err(anyhow::anyhow!("job {} has no process group", job.job_id)),
    };

    finalize_background_resume(shell, job, resume_result)
        .await
        .map_err(|err| {
            error!("BG_CMD_SIGCONT_ERROR: Failed to resume job {job_id}: {err}");
            err
        })?;
    debug!("BG_CMD_SIGCONT_SUCCESS: SIGCONT sent successfully to job {job_id}");
    Ok(BgResumeSuccess {
        job_id,
        cmd: job_cmd,
    })
}

/// Async `bg` driver: parse, reconcile, pre-mutation stable resolution,
/// best-effort multi-target resume.
///
/// A reconciled completed job is never mistaken for a stopped one, and the
/// canonical finalizer (inside [`finalize_background_resume`]) is the only
/// path that retires completion into the known-async ledger — `bg` archives
/// but never consumes ledger status. One failed target never skips or rolls
/// back the others; any failure still makes the whole invocation non-zero.
async fn background_jobs(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    background_jobs_with(shell, ctx, argv, &mut |pgid| {
        killpg(pgid, Signal::SIGCONT).map_err(Into::into)
    })
    .await
}

/// Async `bg` driver with an injectable SIGCONT sender (see
/// [`resume_background_job_with`]); production uses [`background_jobs`].
pub(crate) async fn background_jobs_with<F>(
    shell: &mut Shell,
    ctx: &Context,
    argv: Vec<String>,
    send_cont: &mut F,
) -> Result<()>
where
    F: FnMut(Pid) -> Result<()>,
{
    let operands = parse_bg_operands(&argv)?;
    shell.check_job_state().await?;

    if operands.is_empty() {
        let Some(job_id) = default_bg_target(&shell.wait_jobs) else {
            if shell.wait_jobs.is_empty() {
                return Err(anyhow::anyhow!("there are no suitable jobs"));
            }
            return Err(anyhow::anyhow!("no stopped jobs"));
        };
        let resumed = resume_background_job_with(shell, job_id, send_cont).await?;
        ctx.write_stdout(&format!(
            "dsh: job {} '{}' to background",
            resumed.job_id, resumed.cmd
        ))
        .ok();
        return Ok(());
    }

    let resolutions = resolve_bg_targets(&operands, &shell.wait_jobs);
    let mut failures: Vec<BgFailure> = Vec::new();
    for resolution in resolutions {
        match resolution {
            BgOperandResolution::Invalid { operand, reason } => {
                failures.push(BgFailure { operand, reason });
            }
            BgOperandResolution::Target { operand, job_id } => {
                match resume_background_job_with(shell, job_id, send_cont).await {
                    Ok(resumed) => {
                        ctx.write_stdout(&format!(
                            "dsh: job {} '{}' to background",
                            resumed.job_id, resumed.cmd
                        ))
                        .ok();
                    }
                    Err(err) => failures.push(BgFailure {
                        operand,
                        reason: err.to_string(),
                    }),
                }
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        let detail = failures
            .iter()
            .map(|failure| format!("{}: {}", failure.operand, failure.reason))
            .collect::<Vec<_>>()
            .join("; ");
        Err(anyhow::anyhow!(detail))
    }
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

    fn fg_test_ctx() -> Context {
        Context::new_safe(getpid(), getpgrp(), true)
    }

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

        let result = finalize_foreground_job(&mut shell, job, Ok(()), &fg_test_ctx()).await;
        assert!(
            result.is_err(),
            "Running + Ok(()) is an infrastructure inconsistency, never synthetic success"
        );
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

        let status = finalize_foreground_job(&mut shell, job, Ok(()), &fg_test_ctx())
            .await
            .expect("finalize");
        assert_eq!(
            status,
            crate::process::signal_exit_status(Signal::SIGTSTP),
            "re-stopped fg invocation reports 128 + stop signal"
        );

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

        let status = finalize_foreground_job(&mut shell, job, Ok(()), &fg_test_ctx())
            .await
            .expect("finalize");
        assert_eq!(status, 0);

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
