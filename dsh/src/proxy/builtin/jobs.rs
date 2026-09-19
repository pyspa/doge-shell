//! Job control command handlers (jobs, fg, bg).

use crate::process::ProcessState;
use crate::process::wait::{is_job_completed, is_job_stopped};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use std::borrow::Cow;
use tabled::{Table, Tabled};
use tracing::{debug, error};

mod bg;
pub use bg::execute_bg;
struct Job {
    job: usize,
    pid: i32,
    state: String,
    command: String,
}

impl Tabled for Job {
    const LENGTH: usize = 4;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Owned(self.job.to_string()),
            Cow::Owned(self.pid.to_string()),
            Cow::Borrowed(self.state.as_str()),
            Cow::Borrowed(self.command.as_str()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![
            Cow::Borrowed("job"),
            Cow::Borrowed("pid"),
            Cow::Borrowed("state"),
            Cow::Borrowed("command"),
        ]
    }
}

/// Parse job specification (e.g., "%1", "1", "%+", "%-").
///
/// Returns the job index in wait_jobs vector, or None if not found.
pub fn parse_job_spec(spec: &str, wait_jobs: &[crate::process::Job]) -> Option<usize> {
    if spec.is_empty() {
        // Default to most recent job
        return if wait_jobs.is_empty() {
            None
        } else {
            Some(wait_jobs.len() - 1)
        };
    }

    let spec = spec.trim();

    // Handle %+ (current job) and %- (previous job)
    if spec == "%+" || spec == "+" {
        return if wait_jobs.is_empty() {
            None
        } else {
            Some(wait_jobs.len() - 1)
        };
    }
    if spec == "%-" || spec == "-" {
        return if wait_jobs.len() < 2 {
            None
        } else {
            Some(wait_jobs.len() - 2)
        };
    }

    // Handle %n or n format (job number)
    let job_num_str = if let Some(stripped) = spec.strip_prefix('%') {
        stripped
    } else {
        spec
    };

    if let Ok(job_num) = job_num_str.parse::<usize>() {
        // Find job by job_id
        for (index, job) in wait_jobs.iter().enumerate() {
            if job.job_id == job_num {
                return Some(index);
            }
        }
    }

    None
}

/// Execute the `jobs` builtin command.
///
/// Lists all background jobs.
pub fn execute_jobs(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    if shell.wait_jobs.is_empty() {
        ctx.write_stdout("jobs: there are no jobs")?;
    } else {
        let jobs: Vec<Job> = shell
            .wait_jobs
            .iter()
            .map(|job| Job {
                job: job.job_id,
                pid: job.pid.map(|p| p.as_raw()).unwrap_or(-1),
                state: format!("{}", job.state),
                command: job.cmd.clone(),
            })
            .collect();
        let table = Table::new(jobs).to_string();
        ctx.write_stdout(table.as_str())?;
    }
    Ok(())
}

/// Execute the `fg` builtin command.
///
/// Brings a background job to the foreground.
///
/// Thin sync bridge over [`foreground_selected_job`]: the async driver owns
/// the wait (and its `OutputMonitor` drain), while this boundary only blocks
/// the calling worker thread until it completes.
pub fn execute_fg(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    debug!(
        "FG_CMD_START: Starting fg command - wait_jobs.len(): {}, args: {:?}",
        shell.wait_jobs.len(),
        argv
    );

    if shell.wait_jobs.is_empty() {
        debug!("FG_CMD_NO_JOBS: No jobs available for fg command");
        ctx.write_stdout("fg: there are no suitable jobs")?;
        return Ok(());
    }

    let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
    debug!("FG_CMD_SPEC: Job specification: '{}'", job_spec);

    // Log current job list for debugging
    debug!("FG_CMD_AVAILABLE_JOBS: Current job list:");
    for (i, job) in shell.wait_jobs.iter().enumerate() {
        debug!(
            "FG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
            i, job.job_id, job.pid, job.state, job.foreground, job.cmd
        );
    }

    let Some(job_index) = parse_job_spec(job_spec, &shell.wait_jobs) else {
        let error_msg = if job_spec.is_empty() {
            "fg: no current job".to_string()
        } else {
            format!("fg: job not found: {job_spec}")
        };
        debug!("FG_CMD_NOT_FOUND: {}", error_msg);
        ctx.write_stderr(&error_msg)?;
        return Err(anyhow::anyhow!(error_msg));
    };

    match run_foreground_driver(shell, ctx, job_index) {
        Ok(()) => Ok(()),
        Err(err) => {
            error!("FG_CMD_ERROR: foreground wait failed: {:?}", err);
            ctx.write_stderr(&format!("{err}")).ok();
            Err(err)
        }
    }
}

/// Sync → async bridge for the `fg` foreground driver.
///
/// The shell runs on a multi-thread Tokio runtime, so `block_in_place` hands
/// other tasks to a fresh worker while this thread blocks in `block_on`.
/// Never construct a nested `Runtime` here: that panics inside the existing
/// runtime. When no runtime exists (plain sync callers/tests), a fresh
/// runtime is created instead. `block_in_place` is multi-thread-only, so
/// unit tests must either call [`foreground_selected_job`] directly or run
/// on `#[tokio::test(flavor = "multi_thread")]`.
///
/// The runtime flavor is checked before entering `block_in_place`.
/// Current-thread runtimes are rejected without polling the foreground
/// future, so the job remains in `wait_jobs`. Internal panics inside the
/// foreground future propagate as panics and are never converted into
/// runtime-compatibility errors.
fn run_foreground_driver(shell: &mut Shell, ctx: &Context, job_index: usize) -> Result<()> {
    block_on_fg_future(foreground_selected_job(shell, ctx, job_index))?
}

/// Block a foreground future on the current Tokio runtime when possible.
///
/// Compatibility is decided up front via [`tokio::runtime::Handle::runtime_flavor`],
/// before the future is polled: multi-thread runtimes use
/// `block_in_place` + `block_on`, current-thread (and any unknown future
/// flavor) is rejected with an error, and callers outside any runtime get a
/// fresh `Runtime`. No `catch_unwind` is used here on purpose.
fn block_on_fg_future<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                Ok(tokio::task::block_in_place(|| handle.block_on(future)))
            }
            tokio::runtime::RuntimeFlavor::CurrentThread => Err(anyhow::anyhow!(
                "fg requires a multi-thread Tokio runtime (current_thread is unsupported)"
            )),
            flavor => Err(anyhow::anyhow!(
                "fg does not support Tokio runtime flavor: {flavor:?}"
            )),
        },
        Err(_) => Ok(tokio::runtime::Runtime::new()?.block_on(future)),
    }
}

/// Async single source of truth for `fg` resume.
///
/// Takes temporary ownership of the job from `wait_jobs`, drives the existing
/// async foreground wait (which drains background `OutputMonitor`s), then
/// reconciles the lifecycle via [`finalize_foreground_job`]:
/// completed jobs are dropped, stopped/still-active jobs return to the table
/// with their real observed state.
pub(crate) async fn foreground_selected_job(
    shell: &mut Shell,
    ctx: &Context,
    job_index: usize,
) -> Result<()> {
    if job_index >= shell.wait_jobs.len() {
        return Err(anyhow::anyhow!("fg: no current job"));
    }
    let mut job = shell.wait_jobs.remove(job_index);
    debug!(
        "FG_CMD_SELECTED: Selected job {} at index {} for foreground",
        job.job_id, job_index
    );
    debug!(
        "FG_CMD_JOB_DETAILS: Job details before fg - state: {:?}, pgid: {:?}, pid: {:?}",
        job.state, job.pgid, job.pid
    );

    ctx.write_stdout(&format!(
        "dsh: job {} '{}' to foreground",
        job.job_id, job.cmd
    ))
    .ok();

    // `job.state` can lag the tree (`bg` only flips the summary), so consult
    // the tree as well: a still-stopped pipeline must get SIGCONT.
    let cont = matches!(job.state, ProcessState::Stopped(_, _)) || is_job_stopped(&job);
    if cont {
        debug!(
            "FG_CMD_STOPPED: Job {} is stopped, will send SIGCONT",
            job.job_id
        );
    } else {
        debug!(
            "FG_CMD_NOT_STOPPED: Job {} is not stopped, no SIGCONT needed (state: {:?})",
            job.job_id, job.state
        );
    }

    let old_state = job.state;
    // `job.foreground` stays `false`: it records background provenance, not
    // temporary foreground ownership during this resume.
    job.state = ProcessState::Running;
    debug!(
        "FG_CMD_STATE_CHANGE: Set job {} state from {:?} to Running",
        job.job_id, old_state
    );

    debug!(
        "FG_CMD_FOREGROUND_CALL: About to call put_in_foreground for job {} with no_hang=true, cont={}",
        job.job_id, cont
    );

    // `fg` resumes a job whose own `job.foreground` stayed `false`
    // (it was backgrounded), so `yield_to_foreground_agent` can't
    // read that off `job` the way `shell/eval.rs` does for a
    // command that started in the foreground - pass `true`
    // directly instead. Without this, Ctrl+Z on a recognized agent
    // CLI followed by `fg` would leave this pane stuck reporting
    // `dsh` for the rest of that agent's run. The guard must cover the
    // whole await below.
    let _agent_handoff =
        crate::agent_lifecycle::yield_to_foreground_agent(shell, &job, ctx.interactive, true);

    let job_id = job.job_id;
    let wait_result = job.put_in_foreground(true, cont).await;
    debug!(
        "FG_CMD_WAIT_DONE: put_in_foreground finished for job {}: {:?}",
        job_id,
        wait_result.as_ref().map(|_| ()).map_err(|e| e.to_string())
    );
    finalize_foreground_job(shell, job, wait_result)
}

/// Reconcile a foreground wait and requeue the job when it is still active.
///
/// Refreshes the summary state from the process tree first (real `Stopped`
/// state, never synthesized), then pushes back to `wait_jobs` unless the
/// tree says completed. Retention runs before `wait_result` propagation so
/// an error path never orphans an active process group; a completed job is
/// still dropped even when the wait errored.
pub(crate) fn finalize_foreground_job(
    shell: &mut Shell,
    mut job: crate::process::Job,
    wait_result: Result<()>,
) -> Result<()> {
    job.refresh_lifecycle_state();
    let still_active = !is_job_completed(&job);
    if still_active {
        debug!(
            "FG_CMD_REQUEUE: Job {} still active after foreground wait (state: {:?}), returning to job table",
            job.job_id, job.state
        );
        shell.wait_jobs.push(job);
    } else {
        debug!(
            "FG_CMD_DONE: Job {} completed, not returning to job table",
            job.job_id
        );
    }
    wait_result
}

/// Reconcile a background resume without orphaning an active job on error.
fn finalize_background_resume(
    shell: &mut Shell,
    mut job: crate::process::Job,
    resume_result: Result<()>,
) -> Result<()> {
    if resume_result.is_ok() {
        job.mark_stopped_processes_running();
    } else {
        job.refresh_lifecycle_state();
    }

    if !job.is_process_tree_completed() {
        debug!(
            "BG_CMD_REQUEUE: Job {} remains active after resume attempt (state: {:?})",
            job.job_id, job.state
        );
        shell.wait_jobs.push(job);
    } else {
        debug!(
            "BG_CMD_DONE: Job {} is already completed, not returning to job table",
            job.job_id
        );
    }

    resume_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Job as ProcJob, JobProcess, Process};
    use nix::sys::signal::Signal as NixSignal;
    use nix::unistd::{Pid, getpgrp, getpid};

    fn test_shell() -> Shell {
        Shell::new(crate::environment::Environment::new())
    }

    fn test_ctx() -> Context {
        let mut ctx = Context::new_safe(getpid(), getpgrp(), true);
        ctx.interactive = false;
        ctx
    }

    fn stopped_tree_job(job_id: usize, signal: NixSignal) -> ProcJob {
        let mut job = ProcJob::new("sleep 60".to_string(), getpgrp());
        job.job_id = job_id;
        let pid = Pid::from_raw(424242);
        job.pid = Some(pid);
        job.pgid = Some(pid);
        let mut proc = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
        proc.pid = Some(pid);
        proc.state = ProcessState::Stopped(pid, signal);
        job.set_process(JobProcess::Command(proc));
        // Simulate `fg`'s stale pre-wait summary.
        job.state = ProcessState::Running;
        job
    }

    fn completed_tree_job(job_id: usize) -> ProcJob {
        let mut job = ProcJob::new("true".to_string(), getpgrp());
        job.job_id = job_id;
        let pid = Pid::from_raw(424243);
        job.pid = Some(pid);
        job.pgid = Some(pid);
        let mut proc = Process::new("true".to_string(), vec!["true".to_string()]);
        proc.pid = Some(pid);
        proc.state = ProcessState::Completed(0, None);
        job.set_process(JobProcess::Command(proc));
        job.state = ProcessState::Running;
        job
    }

    fn running_tree_job(job_id: usize) -> ProcJob {
        let mut job = ProcJob::new("sleep 60".to_string(), getpgrp());
        job.job_id = job_id;
        let pid = Pid::from_raw(424244);
        job.pid = Some(pid);
        job.pgid = Some(pid);
        let mut proc = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
        proc.pid = Some(pid);
        proc.state = ProcessState::Running;
        job.set_process(JobProcess::Command(proc));
        job.state = ProcessState::Running;
        job
    }

    #[test]
    fn fg_requeues_job_that_stops_again() {
        let mut shell = test_shell();
        let job_id = 7;
        let pid = Pid::from_raw(424242);
        let job = stopped_tree_job(job_id, NixSignal::SIGTSTP);

        let result = finalize_foreground_job(&mut shell, job, Ok(()));
        assert!(result.is_ok());
        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert_eq!(requeued.job_id, job_id);
        assert_eq!(requeued.pid, Some(pid));
        assert_eq!(
            requeued.state,
            ProcessState::Stopped(pid, NixSignal::SIGTSTP),
            "re-stopped job must keep its real observed stop state, not Running"
        );
        assert_eq!(parse_job_spec("%+", &shell.wait_jobs), Some(0));
        let _ = test_ctx();
    }

    #[test]
    fn fg_requeues_job_stopped_by_sigstop() {
        let mut shell = test_shell();
        let pid = Pid::from_raw(424242);
        let job = stopped_tree_job(8, NixSignal::SIGSTOP);

        finalize_foreground_job(&mut shell, job, Ok(())).expect("finalize");
        assert_eq!(shell.wait_jobs.len(), 1);
        assert_eq!(
            shell.wait_jobs[0].state,
            ProcessState::Stopped(pid, NixSignal::SIGSTOP)
        );
    }

    #[test]
    fn fg_does_not_requeue_completed_job() {
        let mut shell = test_shell();
        let job = completed_tree_job(3);

        let result = finalize_foreground_job(&mut shell, job, Ok(()));
        assert!(result.is_ok());
        assert!(
            shell.wait_jobs.is_empty(),
            "completed job must not return to the job table"
        );
    }

    #[test]
    fn fg_requeues_active_job_after_wait_error() {
        let mut shell = test_shell();
        let job = running_tree_job(9);

        let result = finalize_foreground_job(&mut shell, job, Err(anyhow::anyhow!("boom")));
        assert!(result.is_err(), "primary wait error must propagate");
        assert_eq!(
            shell.wait_jobs.len(),
            1,
            "active job must survive wait error"
        );
        assert_eq!(shell.wait_jobs[0].job_id, 9);
        assert_eq!(shell.wait_jobs[0].state, ProcessState::Running);
    }

    #[test]
    fn fg_does_not_resurrect_completed_job_on_wait_error() {
        let mut shell = test_shell();
        let job = completed_tree_job(11);

        let result = finalize_foreground_job(&mut shell, job, Err(anyhow::anyhow!("boom")));
        assert!(result.is_err());
        assert!(
            shell.wait_jobs.is_empty(),
            "completed job must stay dropped even when the wait errored"
        );
    }

    #[test]
    fn bg_success_marks_stopped_process_tree_running() {
        let mut shell = test_shell();
        let job = stopped_tree_job(12, NixSignal::SIGTSTP);

        finalize_background_resume(&mut shell, job, Ok(())).expect("finalize");

        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert_eq!(requeued.job_id, 12);
        assert_eq!(requeued.state, ProcessState::Running);
        assert_eq!(
            requeued.process.as_deref().map(JobProcess::get_state),
            Some(ProcessState::Running)
        );
    }

    #[test]
    fn bg_sigcont_error_requeues_stopped_job() {
        let mut shell = test_shell();
        let pid = Pid::from_raw(424242);
        let job = stopped_tree_job(13, NixSignal::SIGTTIN);

        let result =
            finalize_background_resume(&mut shell, job, Err(anyhow::anyhow!("SIGCONT failed")));

        assert!(result.is_err());
        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert_eq!(requeued.job_id, 13);
        assert_eq!(
            requeued.state,
            ProcessState::Stopped(pid, NixSignal::SIGTTIN)
        );
        assert_eq!(
            requeued.process.as_deref().map(JobProcess::get_state),
            Some(ProcessState::Stopped(pid, NixSignal::SIGTTIN))
        );
    }

    #[test]
    fn bg_error_does_not_resurrect_completed_job() {
        let mut shell = test_shell();
        let job = completed_tree_job(14);

        let result =
            finalize_background_resume(&mut shell, job, Err(anyhow::anyhow!("SIGCONT failed")));

        assert!(result.is_err());
        assert!(shell.wait_jobs.is_empty());
    }

    #[test]
    fn bg_missing_pgid_keeps_job_stopped() {
        let mut shell = test_shell();
        let pid = Pid::from_raw(424242);
        let mut job = stopped_tree_job(15, NixSignal::SIGTSTP);
        job.pgid = None;
        shell.wait_jobs.push(job);
        let ctx = test_ctx();

        let result = execute_bg(&mut shell, &ctx, vec!["bg".to_string(), "%15".to_string()]);

        let err = result.expect_err("missing pgid must fail");
        assert!(err.to_string().contains("has no process group"));
        assert_eq!(shell.wait_jobs.len(), 1);
        let requeued = &shell.wait_jobs[0];
        assert_eq!(requeued.job_id, 15);
        assert_eq!(
            requeued.state,
            ProcessState::Stopped(pid, NixSignal::SIGTSTP)
        );
        assert_eq!(
            requeued.process.as_deref().map(JobProcess::get_state),
            Some(ProcessState::Stopped(pid, NixSignal::SIGTSTP))
        );
    }

    #[test]
    fn bg_default_selection_uses_process_tree_not_stale_summary() {
        let mut shell = test_shell();
        let mut job = stopped_tree_job(16, NixSignal::SIGSTOP);
        job.pgid = None;
        assert_eq!(job.state, ProcessState::Running);
        assert!(job.has_stopped_process());
        shell.wait_jobs.push(job);
        let ctx = test_ctx();

        let result = execute_bg(&mut shell, &ctx, vec!["bg".to_string()]);

        let err = result.expect_err("selected stopped tree should reach pgid validation");
        assert!(err.to_string().contains("job 16 has no process group"));
        assert_eq!(shell.wait_jobs.len(), 1);
        assert!(shell.wait_jobs[0].has_stopped_process());
    }

    #[test]
    fn bg_explicit_job_rejects_stale_stopped_summary_when_tree_is_running() {
        let mut shell = test_shell();
        let mut job = running_tree_job(17);
        job.state = ProcessState::Stopped(Pid::from_raw(424244), NixSignal::SIGTSTP);
        shell.wait_jobs.push(job);
        let ctx = test_ctx();

        let result = execute_bg(&mut shell, &ctx, vec!["bg".to_string(), "%17".to_string()]);

        let err = result.expect_err("running process tree must not be resumed");
        assert!(err.to_string().contains("already running"));
        assert_eq!(shell.wait_jobs.len(), 1);
        assert_eq!(
            shell.wait_jobs[0]
                .process
                .as_deref()
                .map(JobProcess::get_state),
            Some(ProcessState::Running)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fg_bridge_runs_future_on_multi_thread_runtime() {
        let value =
            block_on_fg_future(async { 42 }).expect("multi-thread runtime should support fg");
        assert_eq!(value, 42);
    }

    #[test]
    fn fg_bridge_rejects_current_thread_without_polling_future() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            let polled = Arc::new(AtomicBool::new(false));
            let marker = polled.clone();
            let result = block_on_fg_future(async move {
                marker.store(true, Ordering::SeqCst);
                123
            });
            let err = result.expect_err("current-thread runtime must be rejected");
            assert!(
                err.to_string().contains("multi-thread Tokio runtime"),
                "unexpected error: {err}"
            );
            assert!(
                !polled.load(Ordering::SeqCst),
                "rejected future must never be polled"
            );
        });
    }

    #[test]
    fn fg_bridge_current_thread_rejection_keeps_job_table_intact() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            let mut shell = test_shell();
            shell.wait_jobs.push(running_tree_job(42));
            let ctx = test_ctx();
            let result = run_foreground_driver(&mut shell, &ctx, 0);
            let err = result.expect_err("current-thread runtime must be rejected");
            assert!(
                err.to_string().contains("multi-thread Tokio runtime"),
                "unexpected error: {err}"
            );
            assert_eq!(
                shell.wait_jobs.len(),
                1,
                "current-thread rejection must happen before fg takes ownership"
            );
            assert_eq!(shell.wait_jobs[0].job_id, 42);
        });
    }

    /// `catch_unwind` here only observes that the panic passes through the
    /// bridge; production `block_on_fg_future` never converts it to an error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fg_bridge_does_not_mask_inner_future_panic() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = block_on_fg_future(async {
                panic!("fg bridge sentinel panic");
            });
        }));
        let payload = result.expect_err("inner panic must propagate, not become a runtime error");
        let message = payload
            .downcast_ref::<&'static str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
        assert_eq!(message, Some("fg bridge sentinel panic"));
    }

    #[test]
    fn fg_bridge_runs_future_outside_existing_runtime() {
        let value =
            block_on_fg_future(async { 42 }).expect("outside-runtime path should build a runtime");
        assert_eq!(value, 42);
    }

    /// Background capture must keep draining while the job runs in the
    /// foreground. Without the async `OutputMonitor` drain the child blocks
    /// on a full pipe and the foreground wait hangs.
    ///
    /// The pipe + `OutputMonitor` layout mirrors what `fork.rs` builds for a
    /// background external (`child stdout/stderr -> capture pipe ->
    /// OutputMonitor`); the child is spawned directly so the test does not
    /// depend on the interactive `setpgid` path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreground_resume_drains_background_capture() {
        use crate::process::io::OutputMonitor;
        use dsh_types::observed_output::ObservedStream;
        use std::os::unix::io::IntoRawFd;
        use std::process::{Command as StdCommand, Stdio};
        use std::time::Duration;

        let mut shell = test_shell();
        let observer = dsh_types::observed_output::ObservedOutput::shared(1024 * 1024);

        // POSIX-only, ~500KiB per stream: well past any pipe capacity, no GNU flags.
        let script = r#"i=0; while [ "$i" -lt 30000 ]; do printf '0123456789abcdef\n'; printf '0123456789abcdef\n' >&2; i=$((i + 1)); done"#;
        let mut child = StdCommand::new("sh")
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn large-output child");
        let child_pid = Pid::from_raw(child.id() as i32);
        let stdout = child.stdout.take().expect("take child stdout");
        let stderr = child.stderr.take().expect("take child stderr");
        // `Child` only holds the pid now; the wait loop reaps via `waitpid`.
        // Dropping here must not kill the child before the drain runs.
        std::mem::forget(child);

        let mut job = ProcJob::new("bg-capture-test".to_string(), getpgrp());
        job.job_id = shell.get_job_id();
        // Provenance stays background: `fg` must not flip this field.
        job.foreground = false;
        job.pid = Some(child_pid);
        job.pgid = Some(child_pid);
        let mut proc = Process::new(
            "sh".to_string(),
            vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        );
        proc.pid = Some(child_pid);
        job.set_process(JobProcess::Command(proc));
        job.monitors.push(OutputMonitor::new(
            stdout.into_raw_fd(),
            Some(observer.clone()),
            ObservedStream::Stdout,
        ));
        job.monitors.push(OutputMonitor::new(
            stderr.into_raw_fd(),
            Some(observer.clone()),
            ObservedStream::Stderr,
        ));
        assert!(!job.monitors.is_empty());

        shell.wait_jobs.push(job);
        assert_eq!(shell.wait_jobs.len(), 1);

        let mut fg_ctx = Context::new_safe(getpid(), getpgrp(), true);
        fg_ctx.interactive = false;

        let wait = tokio::time::timeout(
            Duration::from_secs(20),
            foreground_selected_job(&mut shell, &fg_ctx, 0),
        )
        .await;
        let wait_result = match wait {
            Ok(result) => result,
            Err(_) => {
                // Never orphan the child behind a hung wait: kill and reap
                // before failing so one timeout cannot poison later tests.
                let _ = nix::sys::signal::kill(child_pid, NixSignal::SIGKILL);
                let _ = nix::sys::wait::waitpid(child_pid, None);
                panic!("foreground driver hung: background capture was not drained");
            }
        };
        wait_result.expect("foreground driver failed");

        assert!(
            shell.wait_jobs.is_empty(),
            "completed foreground job must not be requeued"
        );
        // `job.foreground == false` is provenance, not temporary ownership.
        let snapshot = observer.lock().unwrap().snapshot();
        assert!(
            !snapshot.stdout.is_empty(),
            "stdout capture drain must have produced output"
        );
        assert!(
            !snapshot.stderr.is_empty(),
            "stderr capture uses the same drain ownership model"
        );
        // The wait loop reaps known pids; a leftover child would be a leak.
        if let Ok(nix::sys::wait::WaitStatus::StillAlive) =
            nix::sys::wait::waitpid(child_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
        {
            let _ = nix::sys::signal::kill(child_pid, NixSignal::SIGKILL);
            let _ = nix::sys::wait::waitpid(child_pid, None);
            panic!("child survived the foreground wait");
        }
    }
}
