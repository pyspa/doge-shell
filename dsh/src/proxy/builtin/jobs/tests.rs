//! `jobs` builtin regression tests (fg/bg finalizers, bridge).

use super::*;
use crate::process::{Job as ProcJob, JobProcess, Process, ProcessState};
use crate::shell::Shell;
use dsh_types::Context;
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

#[tokio::test]
async fn fg_requeues_job_that_stops_again() {
    let mut shell = test_shell();
    let job_id = 7;
    let pid = Pid::from_raw(424242);
    let job = stopped_tree_job(job_id, NixSignal::SIGTSTP);

    let result = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx()).await;
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

#[tokio::test]
async fn fg_requeues_job_stopped_by_sigstop() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424242);
    let job = stopped_tree_job(8, NixSignal::SIGSTOP);

    finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx())
        .await
        .expect("finalize");
    assert_eq!(shell.wait_jobs.len(), 1);
    assert_eq!(
        shell.wait_jobs[0].state,
        ProcessState::Stopped(pid, NixSignal::SIGSTOP)
    );
}

#[tokio::test]
async fn fg_does_not_requeue_completed_job() {
    let mut shell = test_shell();
    let job = completed_tree_job(3);

    let result = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx()).await;
    assert!(result.is_ok());
    assert!(
        shell.wait_jobs.is_empty(),
        "completed job must not return to the job table"
    );
}

#[tokio::test]
async fn fg_completion_archives_known_async_status() {
    use nix::unistd::Pid;

    let mut shell = test_shell();
    let job = completed_tree_job(21);
    let pid = Pid::from_raw(424243);
    shell.known_async.register(pid, 21);

    finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx())
        .await
        .expect("finalize");

    assert!(shell.wait_jobs.is_empty());
    let status = shell
        .known_async
        .consume_completed(pid)
        .expect("fg completion must archive the async status");
    assert_eq!(status, 0);
}

#[tokio::test]
async fn fg_requeues_active_job_after_wait_error() {
    let mut shell = test_shell();
    let job = running_tree_job(9);

    let result =
        finalize_foreground_job(&mut shell, job, Err(anyhow::anyhow!("boom")), &test_ctx()).await;
    assert!(result.is_err(), "primary wait error must propagate");
    assert_eq!(
        shell.wait_jobs.len(),
        1,
        "active job must survive wait error"
    );
    assert_eq!(shell.wait_jobs[0].job_id, 9);
    assert_eq!(shell.wait_jobs[0].state, ProcessState::Running);
}

#[tokio::test]
async fn fg_does_not_resurrect_completed_job_on_wait_error() {
    let mut shell = test_shell();
    let job = completed_tree_job(11);

    let result =
        finalize_foreground_job(&mut shell, job, Err(anyhow::anyhow!("boom")), &test_ctx()).await;
    assert!(result.is_err());
    assert!(
        shell.wait_jobs.is_empty(),
        "completed job must stay dropped even when the wait errored"
    );
}

#[tokio::test]
async fn bg_success_marks_stopped_process_tree_running() {
    let mut shell = test_shell();
    let job = stopped_tree_job(12, NixSignal::SIGTSTP);

    finalize_background_resume(&mut shell, job, Ok(()))
        .await
        .expect("finalize");

    assert_eq!(shell.wait_jobs.len(), 1);
    let requeued = &shell.wait_jobs[0];
    assert_eq!(requeued.job_id, 12);
    assert_eq!(requeued.state, ProcessState::Running);
    assert_eq!(
        requeued.process.as_deref().map(JobProcess::get_state),
        Some(ProcessState::Running)
    );
}

#[tokio::test]
async fn bg_sigcont_error_requeues_stopped_job() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424242);
    let job = stopped_tree_job(13, NixSignal::SIGTTIN);

    let result =
        finalize_background_resume(&mut shell, job, Err(anyhow::anyhow!("SIGCONT failed"))).await;

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

#[tokio::test]
async fn bg_error_does_not_resurrect_completed_job() {
    let mut shell = test_shell();
    let job = completed_tree_job(14);

    let result =
        finalize_background_resume(&mut shell, job, Err(anyhow::anyhow!("SIGCONT failed"))).await;

    assert!(result.is_err());
    assert!(shell.wait_jobs.is_empty());
}

#[tokio::test]
async fn bg_completion_archives_known_async_status() {
    use nix::unistd::Pid;

    let mut shell = test_shell();
    let job = completed_tree_job(23);
    let pid = Pid::from_raw(424243);
    shell.known_async.register(pid, 23);

    finalize_background_resume(&mut shell, job, Ok(()))
        .await
        .expect("finalize");

    assert!(shell.wait_jobs.is_empty());
    let status = shell
        .known_async
        .consume_completed(pid)
        .expect("bg completion must archive the async status");
    assert_eq!(status, 0);
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
async fn job_control_bridge_runs_future_on_multi_thread_runtime() {
    let value =
        block_on_job_control_future(async { 42 }).expect("multi-thread runtime should support fg");
    assert_eq!(value, 42);
}

#[test]
fn job_control_bridge_rejects_current_thread_without_polling_future() {
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
        let result = block_on_job_control_future(async move {
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
fn job_control_bridge_current_thread_rejection_keeps_job_table_intact() {
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
/// bridge; production `block_on_job_control_future` never converts it to an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_control_bridge_does_not_mask_inner_future_panic() {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = block_on_job_control_future(async {
            panic!("job-control bridge sentinel panic");
        });
    }));
    let payload = result.expect_err("inner panic must propagate, not become a runtime error");
    let message = payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some("job-control bridge sentinel panic"));
}

#[test]
fn job_control_bridge_runs_future_outside_existing_runtime() {
    let value = block_on_job_control_future(async { 42 })
        .expect("outside-runtime path should build a runtime");
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
    job.monitors.push(
        OutputMonitor::new(
            stdout.into(),
            Some(observer.clone()),
            ObservedStream::Stdout,
        )
        .expect("create stdout monitor"),
    );
    job.monitors.push(
        OutputMonitor::new(
            stderr.into(),
            Some(observer.clone()),
            ObservedStream::Stderr,
        )
        .expect("create stderr monitor"),
    );
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

fn stopped_full_proxy_job_for_fg(job_id: usize) -> ProcJob {
    use crate::process::Pty;
    use crate::process::pty::PtyMode;
    let mut job = stopped_tree_job(job_id, NixSignal::SIGTSTP);
    job.pty = Some(Pty::new().expect("test pty"));
    job.pty_mode = Some(PtyMode::FullProxy);
    // Stopped ownership: output stays, input already suspended.
    job.pty_output_task = Some(tokio::spawn(async { Ok("pending".to_string()) }));
    job.pty_input_task = None;
    job
}

/// `fg` on a stopped FullProxy tree requeues with PTY/output kept and no
/// terminal input proxy.
#[tokio::test]
async fn fg_stopped_full_proxy_requeues_with_pty_ownership() {
    let mut shell = test_shell();
    let job = stopped_full_proxy_job_for_fg(31);
    let ctx = test_ctx();

    finalize_foreground_job(&mut shell, job, Ok(()), &ctx)
        .await
        .expect("finalize");
    assert_eq!(shell.wait_jobs.len(), 1);
    let requeued = &shell.wait_jobs[0];
    assert_eq!(requeued.job_id, 31);
    assert!(requeued.pty.is_some(), "PTY must stay with stopped job");
    assert_eq!(
        requeued.pty_mode,
        Some(crate::process::pty::PtyMode::FullProxy)
    );
    assert!(
        requeued.pty_output_task.is_some(),
        "output ownership must stay"
    );
    assert!(
        requeued.pty_input_task.is_none(),
        "input proxy must stay inactive at the prompt"
    );
    crate::process::job_pty::cleanup_pty_tasks(&mut shell.wait_jobs[0]).await;
}

/// `fg` completing a FullProxy job retires every PTY resource, archives the
/// canonical status, and never requeues.
#[tokio::test]
async fn fg_completed_full_proxy_retires_all_resources() {
    use crate::process::Pty;
    use crate::process::pty::PtyMode;
    let mut shell = test_shell();
    let mut job = completed_tree_job(32);
    let pid = Pid::from_raw(424243);
    shell.known_async.register(pid, 32);
    job.pty = Some(Pty::new().expect("test pty"));
    job.pty_mode = Some(PtyMode::FullProxy);
    job.pty_input_task = Some(tokio::spawn(std::future::pending::<()>()));
    job.pty_output_task = Some(tokio::spawn(async { Ok("fg-final".to_string()) }));
    let ctx = test_ctx();

    finalize_foreground_job(&mut shell, job, Ok(()), &ctx)
        .await
        .expect("finalize");
    assert!(
        shell.wait_jobs.is_empty(),
        "completed fg job must not return to the table"
    );
    let status = shell
        .known_async
        .consume_completed(pid)
        .expect("fg completion must archive the async status");
    assert_eq!(status, 0);
}

/// Wait errors on a FullProxy job still suspend terminal input before
/// requeueing, so the prompt never shares `/dev/tty` with the job.
#[tokio::test]
async fn fg_wait_error_suspends_full_proxy_input_before_requeue() {
    use crate::process::Pty;
    use crate::process::pty::PtyMode;
    let mut shell = test_shell();
    let mut job = running_tree_job(33);
    job.pty = Some(Pty::new().expect("test pty"));
    job.pty_mode = Some(PtyMode::FullProxy);
    job.pty_output_task = Some(tokio::spawn(async { Ok("pending".to_string()) }));
    job.pty_input_task = Some(tokio::spawn(std::future::pending::<()>()));
    let ctx = test_ctx();

    let result = finalize_foreground_job(&mut shell, job, Err(anyhow::anyhow!("boom")), &ctx).await;
    assert!(result.is_err(), "primary wait error must propagate");
    assert_eq!(shell.wait_jobs.len(), 1);
    let requeued = &shell.wait_jobs[0];
    assert!(requeued.pty.is_some());
    assert!(requeued.pty_output_task.is_some());
    assert!(
        requeued.pty_input_task.is_none(),
        "input proxy must stop even on wait error"
    );
    crate::process::job_pty::cleanup_pty_tasks(&mut shell.wait_jobs[0]).await;
}
