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
    completed_tree_job_with_status(job_id, 0)
}

fn completed_tree_job_with_status(job_id: usize, code: u8) -> ProcJob {
    let mut job = ProcJob::new("true".to_string(), getpgrp());
    job.job_id = job_id;
    let pid = Pid::from_raw(424243);
    job.pid = Some(pid);
    job.pgid = Some(pid);
    let mut proc = Process::new("true".to_string(), vec!["true".to_string()]);
    proc.pid = Some(pid);
    proc.state = ProcessState::Completed(code, None);
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
    assert_eq!(
        result.unwrap(),
        crate::process::signal_exit_status(NixSignal::SIGTSTP)
    );
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

    let status = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx())
        .await
        .expect("finalize");
    assert_eq!(
        status,
        crate::process::signal_exit_status(NixSignal::SIGSTOP)
    );
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
    assert_eq!(result.unwrap(), 0);
    assert!(
        shell.wait_jobs.is_empty(),
        "completed job must not return to the job table"
    );
}

#[tokio::test]
async fn fg_completed_nonzero_reports_command_status() {
    let mut shell = test_shell();
    let job = completed_tree_job_with_status(30, 7);

    let status = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx())
        .await
        .expect("finalize");
    assert_eq!(status, 7);
    assert!(
        shell.wait_jobs.is_empty(),
        "completed job must not return to the job table"
    );
}

#[tokio::test]
async fn fg_signal_completion_reports_128_plus_signal() {
    let mut shell = test_shell();
    let mut job = ProcJob::new("killed".to_string(), getpgrp());
    job.job_id = 31;
    let pid = Pid::from_raw(424243);
    job.pid = Some(pid);
    job.pgid = Some(pid);
    let mut proc = Process::new("killed".to_string(), vec!["killed".to_string()]);
    proc.pid = Some(pid);
    proc.state = ProcessState::signaled(NixSignal::SIGTERM);
    job.set_process(JobProcess::Command(proc));
    job.state = ProcessState::Running;

    let status = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx())
        .await
        .expect("finalize");
    assert_eq!(
        status,
        crate::process::signal_exit_status(NixSignal::SIGTERM)
    );
    assert!(shell.wait_jobs.is_empty());
}

#[tokio::test]
async fn fg_running_with_successful_wait_is_infrastructure_error() {
    let mut shell = test_shell();
    let job = running_tree_job(34);

    let result = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx()).await;
    assert!(
        result.is_err(),
        "Running + Ok(()) must never synthesize status 0"
    );
    assert_eq!(shell.wait_jobs.len(), 1, "job must be requeued first");
    assert_eq!(shell.wait_jobs[0].job_id, 34);
    assert_eq!(shell.wait_jobs[0].state, ProcessState::Running);
}

#[tokio::test]
async fn fg_completion_archives_known_async_status() {
    use nix::unistd::Pid;

    let mut shell = test_shell();
    let job = completed_tree_job(21);
    let pid = Pid::from_raw(424243);
    shell.known_async.register(pid, 21);

    let status = finalize_foreground_job(&mut shell, job, Ok(()), &test_ctx())
        .await
        .expect("finalize");
    assert_eq!(status, 0);

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

/// `wait`-only job-spec surface: `%`-prefixed syntax parses without a
/// table, bare decimals never do (`wait 123` is PID 123).
#[test]
fn wait_percent_job_spec_accepts_only_percent_forms() {
    assert_eq!(parse_percent_job_spec("%1"), Some(JobSpec::Number(1)));
    assert_eq!(parse_percent_job_spec("%42"), Some(JobSpec::Number(42)));
    assert_eq!(parse_percent_job_spec("%+"), Some(JobSpec::Current));
    assert_eq!(parse_percent_job_spec("%%"), Some(JobSpec::Current));
    assert_eq!(parse_percent_job_spec("%-"), Some(JobSpec::Previous));

    // Bare forms are PIDs for `wait`, never job specs.
    assert_eq!(parse_percent_job_spec("1"), None);
    assert_eq!(parse_percent_job_spec("+"), None);
    assert_eq!(parse_percent_job_spec("-"), None);
    // Not job numbers at all.
    assert_eq!(parse_percent_job_spec("%foo"), None);
    assert_eq!(parse_percent_job_spec("%?foo"), None);
    assert_eq!(parse_percent_job_spec("%"), None);
    assert_eq!(parse_percent_job_spec(""), None);
}

/// Active-table resolution: `%+`/`%%` is the last job, `%-` the one
/// before, `%N` the job with that stable number.
#[test]
fn wait_active_job_spec_resolves_current_previous_number() {
    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(running_tree_job(2));
    shell.wait_jobs.push(running_tree_job(3));

    assert_eq!(
        resolve_active_job_spec(JobSpec::Current, &shell.wait_jobs),
        Some(2)
    );
    assert_eq!(
        resolve_active_job_spec(JobSpec::Previous, &shell.wait_jobs),
        Some(1)
    );
    assert_eq!(
        resolve_active_job_spec(JobSpec::Number(1), &shell.wait_jobs),
        Some(0)
    );
    assert_eq!(
        resolve_active_job_spec(JobSpec::Number(99), &shell.wait_jobs),
        None
    );
}

/// Legacy `fg`/`bg` behavior is unchanged, plus `%%` as a current alias.
#[test]
fn legacy_parse_job_spec_keeps_fg_bg_behavior() {
    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(running_tree_job(2));

    assert_eq!(parse_job_spec("1", &shell.wait_jobs), Some(0));
    assert_eq!(parse_job_spec("%1", &shell.wait_jobs), Some(0));
    assert_eq!(parse_job_spec("%2", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("+", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("%+", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("%%", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("-", &shell.wait_jobs), Some(0));
    assert_eq!(parse_job_spec("%-", &shell.wait_jobs), Some(0));
    assert_eq!(parse_job_spec("", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("%99", &shell.wait_jobs), None);
    assert_eq!(parse_job_spec("%foo", &shell.wait_jobs), None);
}

fn wait_argv(args: &[&str]) -> Vec<String> {
    std::iter::once("wait".to_string())
        .chain(args.iter().map(|arg| arg.to_string()))
        .collect()
}

/// Bare decimals are PIDs even when a job owns that number: with
/// `job_id == 123` active, `wait 123` must report unknown-PID 127 and
/// leave the job untouched.
#[test]
fn wait_bare_decimal_is_pid_never_job_number() {
    let mut shell = test_shell();
    let mut job = running_tree_job(123);
    // Distinct associated PID so the two namespaces cannot coincide.
    job.pid = Some(Pid::from_raw(424250));
    shell.wait_jobs.push(job);
    let ctx = test_ctx();

    let status =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["123"])).expect("wait executes");
    assert_eq!(status, 127, "bare 123 must be PID 123, not job 123");
    assert_eq!(shell.wait_jobs.len(), 1, "job 123 must stay owned");
    assert_eq!(shell.wait_jobs[0].job_id, 123);
}

/// Explicit `%N` reaches a retained completed status after the heavy `Job`
/// is gone (e.g. post-`jobs` reconciliation): status is served and the
/// ledger entry is consumed exactly once.
#[test]
fn wait_percent_number_serves_completed_ledger_status() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424260);
    shell.known_async.register(pid, 7);
    assert!(shell.known_async.mark_completed(pid, 3));
    let ctx = test_ctx();

    let status =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["%7"])).expect("wait executes");
    assert_eq!(status, 3);
    assert!(
        shell.known_async.consume_completed(pid).is_none(),
        "consumed exactly once"
    );
}

/// Unknown job specs report 127 without touching owned jobs.
#[test]
fn wait_unknown_job_spec_reports_127() {
    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    let ctx = test_ctx();

    let status =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["%99"])).expect("wait executes");
    assert_eq!(status, 127);
    assert_eq!(shell.wait_jobs.len(), 1);
    let mut empty = test_shell();
    let status =
        super::wait::execute_wait(&mut empty, &ctx, wait_argv(&["%%"])).expect("wait executes");
    assert_eq!(status, 127);
}

/// `wait -p`/`-f` stay scope-out: usage error, not a silent fallback.
#[test]
fn wait_p_and_f_options_stay_unsupported() {
    let mut shell = test_shell();
    let ctx = test_ctx();

    for args in [
        vec!["-p", "done"],
        vec!["-f"],
        vec!["-n", "-p", "done"],
        vec!["-np"],
        vec!["-nx"],
    ] {
        let argv: Vec<String> = std::iter::once("wait".to_string())
            .chain(args.iter().map(|arg| arg.to_string()))
            .collect();
        let err = super::wait::execute_wait(&mut shell, &ctx, argv).expect_err("must reject");
        assert!(
            err.to_string().contains("unsupported option"),
            "unexpected error: {err}"
        );
    }
}

/// `wait -n` with no known jobs reports 127; bare `wait` with no known
/// jobs reports 0. The two arities differ by contract.
#[test]
fn wait_next_with_no_targets_reports_127_while_bare_wait_reports_0() {
    let ctx = test_ctx();

    let mut shell = test_shell();
    let next =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-n"])).expect("wait executes");
    assert_eq!(next, 127);

    // Bundled short flags (`-nn`) behave like repeated `-n`.
    let mut shell = test_shell();
    let bundled =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-nn"])).expect("wait executes");
    assert_eq!(bundled, 127);

    let mut shell = test_shell();
    let bare = super::wait::execute_wait(&mut shell, &ctx, wait_argv(&[])).expect("wait executes");
    assert_eq!(bare, 0);
}

/// `wait -n` serves an already-completed ledger status immediately: the
/// selected status is consumed, the still-`Active` target is untouched.
#[test]
fn wait_next_serves_completed_fast_path_and_keeps_the_rest() {
    let mut shell = test_shell();
    let done = Pid::from_raw(424261);
    let live = Pid::from_raw(424262);
    shell.known_async.register(done, 1);
    assert!(shell.known_async.mark_completed(done, 7));
    shell.known_async.register(live, 2);
    let ctx = test_ctx();

    let status =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-n"])).expect("wait executes");
    assert_eq!(status, 7);
    assert!(
        shell.known_async.consume_completed(done).is_none(),
        "selected status consumed exactly once"
    );
    assert!(
        shell.known_async.active_entry(live).is_some(),
        "unselected Active ownership retained"
    );
}

/// Multiple completed targets resolve deterministically in target order;
/// the unselected status stays retained for a later `wait`.
#[test]
fn wait_next_selects_first_completed_target_in_order() {
    let mut shell = test_shell();
    let first = Pid::from_raw(424263);
    let second = Pid::from_raw(424264);
    shell.known_async.register(first, 1);
    assert!(shell.known_async.mark_completed(first, 1));
    shell.known_async.register(second, 2);
    assert!(shell.known_async.mark_completed(second, 7));
    let ctx = test_ctx();

    let status = super::wait::execute_wait(
        &mut shell,
        &ctx,
        wait_argv(&[
            "-n",
            &first.as_raw().to_string(),
            &second.as_raw().to_string(),
        ]),
    )
    .expect("wait executes");
    assert_eq!(status, 1, "target order wins, not registration luck");
    assert_eq!(
        shell.known_async.consume_completed(second),
        Some(7),
        "unselected status retained for a later wait"
    );
}

/// Duplicate spellings of one target (`wait -n PID PID`) consume once.
#[test]
fn wait_next_deduplicates_pid_targets() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424265);
    shell.known_async.register(pid, 1);
    assert!(shell.known_async.mark_completed(pid, 3));
    let ctx = test_ctx();
    let raw = pid.as_raw().to_string();

    let status = super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-n", &raw, &raw]))
        .expect("wait executes");
    assert_eq!(status, 3);
    assert!(
        shell.known_async.consume_completed(pid).is_none(),
        "no double consume"
    );
}

/// Unknown + valid: the unknown operand is diagnosed but the valid target
/// is still waited on. All-unknown reports 127 without touching `waitpid`.
#[test]
fn wait_next_ignores_unknown_targets_when_a_valid_one_exists() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424266);
    shell.known_async.register(pid, 1);
    assert!(shell.known_async.mark_completed(pid, 1));
    let ctx = test_ctx();

    let status = super::wait::execute_wait(
        &mut shell,
        &ctx,
        wait_argv(&["-n", "999999", &pid.as_raw().to_string()]),
    )
    .expect("wait executes");
    assert_eq!(status, 1);

    let mut shell = test_shell();
    let status =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-n", "999998", "999999"]))
            .expect("wait executes");
    assert_eq!(status, 127, "all-unknown wait-any reports 127");
}

/// A `wait -n` selection is consumed exactly once: a later `wait` on the
/// same PID reports 127.
#[test]
fn wait_next_consumes_exactly_once() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424267);
    shell.known_async.register(pid, 1);
    assert!(shell.known_async.mark_completed(pid, 4));
    let ctx = test_ctx();
    let raw = pid.as_raw().to_string();

    let first = super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-n", &raw]))
        .expect("wait executes");
    assert_eq!(first, 4);
    let second =
        super::wait::execute_wait(&mut shell, &ctx, wait_argv(&[&raw])).expect("wait executes");
    assert_eq!(second, 127);
}

/// `wait -n` prefers the completed target over a stopped one: stops are
/// never completions, and the stopped job stays owned.
#[test]
fn wait_next_prefers_completed_over_stopped_target() {
    let mut shell = test_shell();
    let stopped_pid = Pid::from_raw(424268);
    let mut stopped = stopped_tree_job(1, NixSignal::SIGTSTP);
    stopped.pid = Some(stopped_pid);
    shell.wait_jobs.push(stopped);
    shell.known_async.register(stopped_pid, 1);
    let done = Pid::from_raw(424269);
    shell.known_async.register(done, 2);
    assert!(shell.known_async.mark_completed(done, 7));
    let ctx = test_ctx();

    let status = super::wait::execute_wait(
        &mut shell,
        &ctx,
        wait_argv(&[
            "-n",
            &stopped_pid.as_raw().to_string(),
            &done.as_raw().to_string(),
        ]),
    )
    .expect("wait executes");
    assert_eq!(status, 7, "stopped job must not be selected");
    assert_eq!(shell.wait_jobs.len(), 1, "stopped job stays owned");
    assert_eq!(
        shell.known_async.consume_completed(done),
        None,
        "selected status consumed"
    );
}

/// An `Active` ledger entry with no table job is stale ownership, never a
/// synthetic completion: `wait -n` drops it and reports 127 instead of
/// inventing an exit status from `ECHILD`.
#[test]
fn wait_next_treats_stale_active_entry_as_unknown_not_completion() {
    let mut shell = test_shell();
    let pid = Pid::from_raw(424270);
    // Never our child and never recycled under us: deterministically
    // ECHILD, so no timing is involved.
    shell.known_async.register(pid, 1);
    let ctx = test_ctx();
    let raw = pid.as_raw().to_string();

    let status = super::wait::execute_wait(&mut shell, &ctx, wait_argv(&["-n", &raw]))
        .expect("wait executes");
    assert_eq!(status, 127);
    assert!(
        shell.known_async.active_entry(pid).is_none(),
        "stale ownership pruned"
    );
}

/// `wait -n` never reaps an unrelated child: only the resolved target's
/// canonical PIDs are polled, so a concurrently-live foreign child keeps
/// its status for its real owner (`waitpid(-1)` would steal it).
#[test]
fn wait_next_does_not_reap_unrelated_completion_child() {
    use std::process::{Command as StdCommand, Stdio};

    let unrelated = StdCommand::new("sh")
        .arg("-c")
        .arg("printf unrelated")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn unrelated child");
    let mut job_child = StdCommand::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn job child");

    let mut shell = test_shell();
    let job_pid = Pid::from_raw(job_child.id() as i32);
    let mut job = ProcJob::new("test".to_string(), getpgrp());
    job.job_id = 1;
    job.foreground = false;
    let mut process = Process::new("sh".to_string(), vec![]);
    process.pid = Some(job_pid);
    job.pid = Some(job_pid);
    job.set_process(JobProcess::Command(process));
    shell.known_async.register(job_pid, 1);
    shell.wait_jobs.push(job);
    let ctx = test_ctx();

    let status = super::wait::execute_wait(
        &mut shell,
        &ctx,
        wait_argv(&["-n", &job_pid.as_raw().to_string()]),
    )
    .expect("wait executes");
    assert_eq!(status, 0);

    let output = unrelated.wait_with_output().expect("wait unrelated child");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "unrelated");
    let _ = job_child.wait();
}

/// `wait -n` keeps draining its targets' output monitors while polling.
/// Without the drain the child blocks on a full pipe and the wait-any
/// never observes its termination (same ownership model as the
/// foreground-resume drain test, through the `wait -n` entry point).
///
/// POSIX-only, ~500KiB per stream: well past any pipe capacity. A
/// watchdog SIGKILLs the child after 20s so a drain regression fails
/// with a wrong status instead of hanging the suite; the kill lands as
/// ESRCH when the wait already reaped the child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_next_drains_large_background_capture() {
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
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        let _ = nix::sys::signal::kill(child_pid, NixSignal::SIGKILL);
    });

    let mut job = ProcJob::new("wait-n-capture-test".to_string(), getpgrp());
    job.job_id = shell.get_job_id();
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

    let job_id = job.job_id;
    shell.known_async.register(child_pid, job_id);
    shell.wait_jobs.push(job);

    let mut ctx = Context::new_safe(getpid(), getpgrp(), true);
    ctx.interactive = false;

    let status = super::wait::execute_wait(
        &mut shell,
        &ctx,
        wait_argv(&["-n", &child_pid.as_raw().to_string()]),
    )
    .expect("wait executes");
    assert_eq!(
        status, 0,
        "a drain regression leaves the child pipe-blocked (watchdog SIGKILL would report 137)"
    );

    assert!(
        shell.wait_jobs.is_empty(),
        "completed wait-any selection must not be requeued"
    );
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
        panic!("child survived the wait-any selection");
    }
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
    let status = wait_result.expect("foreground driver failed");
    assert_eq!(status, 0);

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

    let status = finalize_foreground_job(&mut shell, job, Ok(()), &ctx)
        .await
        .expect("finalize");
    assert_eq!(
        status,
        crate::process::signal_exit_status(NixSignal::SIGTSTP)
    );
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

    let status = finalize_foreground_job(&mut shell, job, Ok(()), &ctx)
        .await
        .expect("finalize");
    assert_eq!(status, 0);
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

// --- `jobs` CLI contract (§43): pure parser ---

fn jobs_argv(args: &[&str]) -> Vec<String> {
    std::iter::once("jobs".to_string())
        .chain(args.iter().map(|arg| arg.to_string()))
        .collect()
}

#[test]
fn jobs_parser_accepts_default_and_modes() {
    use super::list::{JobsInvocation, JobsOutputMode, parse_jobs_invocation};

    assert_eq!(
        parse_jobs_invocation(&jobs_argv(&[])).expect("parse"),
        JobsInvocation {
            mode: JobsOutputMode::Default,
            jobspec: None,
        }
    );
    for args in [["-l"], ["--list"]] {
        assert_eq!(
            parse_jobs_invocation(&jobs_argv(&args)).expect("parse"),
            JobsInvocation {
                mode: JobsOutputMode::Long,
                jobspec: None,
            },
            "{args:?}"
        );
    }
    for args in [["-p"], ["--pgid"]] {
        assert_eq!(
            parse_jobs_invocation(&jobs_argv(&args)).expect("parse"),
            JobsInvocation {
                mode: JobsOutputMode::PgidOnly,
                jobspec: None,
            },
            "{args:?}"
        );
    }
    // Repeated same-mode bundles are allowed.
    for args in [["-ll"], ["-pp"]] {
        parse_jobs_invocation(&jobs_argv(&args)).expect("repeated bundle must parse");
    }
}

#[test]
fn jobs_parser_accepts_jobspec_operand() {
    use super::list::{JobsOutputMode, parse_jobs_invocation};

    let parsed = parse_jobs_invocation(&jobs_argv(&["-p", "%3"])).expect("parse");
    assert_eq!(parsed.mode, JobsOutputMode::PgidOnly);
    assert_eq!(parsed.jobspec.as_deref(), Some("%3"));

    let parsed = parse_jobs_invocation(&jobs_argv(&["--", "%3"])).expect("parse");
    assert_eq!(parsed.mode, JobsOutputMode::Default);
    assert_eq!(parsed.jobspec.as_deref(), Some("%3"));

    // `-` / `+` are job aliases, never options.
    for alias in ["-", "+"] {
        let parsed = parse_jobs_invocation(&jobs_argv(&[alias])).expect("parse");
        assert_eq!(parsed.jobspec.as_deref(), Some(alias));
    }
}

#[test]
fn jobs_parser_rejects_bad_options_and_extra_operands() {
    use super::list::parse_jobs_invocation;

    for args in [
        vec!["--bad"],
        vec!["-x"],
        vec!["-z"],
        vec!["--unknown"],
        vec!["%1", "%2"],
        vec!["-l", "-p"],
        vec!["-lp"],
        vec!["-pl"],
    ] {
        let argv = jobs_argv(&args);
        let err = parse_jobs_invocation(&argv).expect_err("must reject {args:?}");
        assert!(
            !err.to_string().starts_with("jobs:"),
            "core error must stay prefixless: {err}"
        );
    }
    for args in [vec!["--bad"], vec!["-x"]] {
        let err = parse_jobs_invocation(&jobs_argv(&args)).expect_err("must reject");
        assert!(
            err.to_string().contains("unsupported option"),
            "unexpected error: {err}"
        );
    }
}

// --- `jobs` rendering (§44): pure helpers ---

#[test]
fn jobs_default_table_omits_pid_column() {
    use super::list::render_jobs_default;

    let job = running_tree_job(1);
    let rendered = render_jobs_default(&[&job]);
    assert!(rendered.contains("job"), "header missing: {rendered}");
    assert!(rendered.contains("state"), "header missing: {rendered}");
    assert!(rendered.contains("command"), "header missing: {rendered}");
    assert!(
        !rendered.contains("pid"),
        "default table must not show pid: {rendered}"
    );
}

#[test]
fn jobs_long_table_includes_pid_column() {
    use super::list::render_jobs_long;

    let job = running_tree_job(1);
    let rendered = render_jobs_long(&[&job]);
    assert!(rendered.contains("pid"), "long table needs pid: {rendered}");
    assert!(
        rendered.contains(&job.pid.expect("pid").as_raw().to_string()),
        "long table needs the pid value: {rendered}"
    );
}

#[test]
fn jobs_pgid_output_is_raw_numbers_without_header() {
    use super::list::render_jobs_pgids;

    let first = running_tree_job(1);
    let second = running_tree_job(2);
    let rendered = render_jobs_pgids(&[&first, &second]).expect("pgids");
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert!(
            line.parse::<i32>().is_ok(),
            "pgid line must be a raw integer: {line:?}"
        );
    }
    assert!(!rendered.contains("job"), "no header: {rendered:?}");
    assert!(!rendered.contains("pid"), "no header: {rendered:?}");

    assert_eq!(
        render_jobs_pgids(&[]).expect("empty"),
        "",
        "no jobs means empty output, not prose"
    );
}

#[test]
fn jobs_pgid_output_fails_closed_without_process_group() {
    use super::list::render_jobs_pgids;

    let mut job = running_tree_job(4);
    job.pgid = None;
    let err = render_jobs_pgids(&[&job]).expect_err("missing pgid must fail");
    assert!(
        err.to_string().contains("has no process group"),
        "unexpected error: {err}"
    );
}

// --- `jobs` jobspec filtering (§45) ---

#[test]
fn jobs_filters_single_target_through_active_table() {
    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(running_tree_job(2));
    shell.wait_jobs.push(running_tree_job(3));
    let ctx = test_ctx();

    // `%1` selects only job 1; `%99` is an error, never a silent empty table.
    super::list::execute_jobs(&mut shell, &ctx, jobs_argv(&["%1"])).expect("filter");
    let err = super::list::execute_jobs(&mut shell, &ctx, jobs_argv(&["%99"]))
        .expect_err("unknown jobspec must fail");
    assert!(
        err.to_string().contains("job not found"),
        "unexpected error: {err}"
    );
    assert!(
        !err.to_string().starts_with("jobs:"),
        "core error must stay prefixless: {err}"
    );
    // Failed filtering never drops owned jobs.
    assert_eq!(shell.wait_jobs.len(), 3);
}

#[test]
fn jobs_current_previous_aliases_resolve_on_active_table() {
    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(running_tree_job(2));
    shell.wait_jobs.push(running_tree_job(3));

    assert_eq!(parse_job_spec("%1", &shell.wait_jobs), Some(0));
    assert_eq!(parse_job_spec("%+", &shell.wait_jobs), Some(2));
    assert_eq!(parse_job_spec("%%", &shell.wait_jobs), Some(2));
    assert_eq!(parse_job_spec("%-", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("-", &shell.wait_jobs), Some(1));
    assert_eq!(parse_job_spec("+", &shell.wait_jobs), Some(2));
    assert_eq!(parse_job_spec("%99", &shell.wait_jobs), None);
}

// --- `bg` stable selection + multi-target orchestration (§46-§54) ---

fn stopped_bg_job(job_id: usize, pid_raw: i32) -> ProcJob {
    let mut job = ProcJob::new(format!("sleep {job_id}"), getpgrp());
    job.job_id = job_id;
    let pid = Pid::from_raw(pid_raw);
    job.pid = Some(pid);
    job.pgid = Some(pid);
    let mut proc = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
    proc.pid = Some(pid);
    proc.state = ProcessState::Stopped(pid, NixSignal::SIGTSTP);
    job.set_process(JobProcess::Command(proc));
    // Stale summary on purpose: the tree, not `job.state`, decides.
    job.state = ProcessState::Running;
    job
}

fn bg_argv(args: &[&str]) -> Vec<String> {
    std::iter::once("bg".to_string())
        .chain(args.iter().map(|arg| arg.to_string()))
        .collect()
}

/// `bg %- %+` must pin both operands before any mutation: after job 2 is
/// removed/resumed/requeued, `%+` still means job 3, never the requeued job 2.
#[test]
fn bg_resolves_all_operands_before_mutation() {
    use super::bg::{BgOperandResolution, resolve_bg_targets};

    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(running_tree_job(2));
    shell.wait_jobs.push(running_tree_job(3));

    assert_eq!(
        resolve_bg_targets(&["%-".to_string(), "%+".to_string()], &shell.wait_jobs),
        vec![
            BgOperandResolution::Target {
                operand: "%-".to_string(),
                job_id: 2,
            },
            BgOperandResolution::Target {
                operand: "%+".to_string(),
                job_id: 3,
            },
        ]
    );
}

#[test]
fn bg_operand_parser_accepts_specs_and_separator() {
    use super::bg::parse_bg_operands;

    assert_eq!(
        parse_bg_operands(&bg_argv(&["%1", "%2"])).expect("parse"),
        vec!["%1".to_string(), "%2".to_string()]
    );
    assert_eq!(
        parse_bg_operands(&bg_argv(&["--", "%1"])).expect("parse"),
        vec!["%1".to_string()]
    );
    // `-` is the previous-job alias, not an option.
    assert_eq!(
        parse_bg_operands(&bg_argv(&["-"])).expect("parse"),
        vec!["-".to_string()]
    );
    for args in [vec!["-x"], vec!["--bogus"]] {
        let err = parse_bg_operands(&bg_argv(&args)).expect_err("must reject {args:?}");
        assert!(
            err.to_string().contains("unsupported option"),
            "unexpected error: {err}"
        );
        assert!(
            !err.to_string().starts_with("bg:"),
            "core error must stay prefixless: {err}"
        );
    }
}

#[test]
fn bg_resolver_maps_every_legacy_spelling() {
    use super::bg::{BgOperandResolution, resolve_bg_targets};

    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(running_tree_job(2));

    for operand in ["%1", "1", "%+", "+", "%%", "%-", "-"] {
        let resolved = resolve_bg_targets(&[operand.to_string()], &shell.wait_jobs);
        assert!(
            matches!(resolved[..], [BgOperandResolution::Target { .. }]),
            "{operand} must resolve"
        );
    }
    for operand in ["%999", "foo"] {
        let resolved = resolve_bg_targets(&[operand.to_string()], &shell.wait_jobs);
        assert!(
            matches!(resolved[..], [BgOperandResolution::Invalid { .. }]),
            "{operand} must not resolve"
        );
    }
}

#[tokio::test]
async fn bg_multi_target_resumes_every_stopped_job() {
    use super::bg::background_jobs_with;

    let mut shell = test_shell();
    shell.wait_jobs.push(stopped_bg_job(1, 426001));
    shell.wait_jobs.push(stopped_bg_job(2, 426002));
    let ctx = test_ctx();

    let mut seen = Vec::new();
    background_jobs_with(
        &mut shell,
        &ctx,
        bg_argv(&["%1", "%2"]),
        &mut |pgid: Pid| {
            seen.push(pgid);
            Ok(())
        },
    )
    .await
    .expect("both targets resume");

    assert_eq!(seen.len(), 2, "one SIGCONT per target");
    assert_eq!(shell.wait_jobs.len(), 2);
    assert!(
        shell.wait_jobs.iter().all(|job| !job.has_stopped_process()),
        "both trees must be running"
    );
    assert!(
        shell
            .wait_jobs
            .iter()
            .all(|job| job.state == ProcessState::Running),
        "both summaries must be running"
    );
}

#[tokio::test]
async fn bg_marker_drift_regression_percent_minus_plus() {
    use super::bg::background_jobs_with;

    let mut shell = test_shell();
    shell.wait_jobs.push(stopped_bg_job(1, 426011));
    shell.wait_jobs.push(stopped_bg_job(2, 426012));
    shell.wait_jobs.push(stopped_bg_job(3, 426013));
    let ctx = test_ctx();

    let mut seen = Vec::new();
    background_jobs_with(
        &mut shell,
        &ctx,
        bg_argv(&["%-", "%+"]),
        &mut |pgid: Pid| {
            seen.push(pgid);
            Ok(())
        },
    )
    .await
    .expect("both targets resume");

    // Exactly jobs 2 and 3 resumed once each: `%+` never drifts onto the
    // requeued job 2.
    seen.sort();
    assert_eq!(seen, vec![Pid::from_raw(426012), Pid::from_raw(426013)]);
    for job in &shell.wait_jobs {
        if job.job_id == 1 {
            assert!(
                job.has_stopped_process(),
                "untargeted job 1 must stay stopped"
            );
        } else {
            assert!(
                !job.has_stopped_process(),
                "job {} must be running",
                job.job_id
            );
        }
    }
}

#[tokio::test]
async fn bg_invalid_first_target_does_not_block_valid_sibling() {
    use super::bg::background_jobs_with;

    let mut shell = test_shell();
    shell.wait_jobs.push(stopped_bg_job(1, 426021));
    shell.wait_jobs.push(stopped_bg_job(2, 426022));
    let ctx = test_ctx();

    let mut seen = Vec::new();
    let err = background_jobs_with(
        &mut shell,
        &ctx,
        bg_argv(&["%99", "%2"]),
        &mut |pgid: Pid| {
            seen.push(pgid);
            Ok(())
        },
    )
    .await
    .expect_err("overall status must be non-zero");
    assert!(
        err.to_string().contains("%99"),
        "aggregate error keeps operand order: {err}"
    );
    assert_eq!(
        seen,
        vec![Pid::from_raw(426022)],
        "valid sibling still resumed"
    );
    assert!(
        !shell
            .wait_jobs
            .iter()
            .find(|job| job.job_id == 2)
            .expect("job 2 owned")
            .has_stopped_process()
    );
}

#[tokio::test]
async fn bg_missing_pgid_sibling_failure_keeps_ownership() {
    use super::bg::background_jobs_with;

    let mut shell = test_shell();
    let mut no_pgid = stopped_bg_job(1, 426031);
    no_pgid.pgid = None;
    shell.wait_jobs.push(no_pgid);
    shell.wait_jobs.push(stopped_bg_job(2, 426032));
    let ctx = test_ctx();

    let mut seen = Vec::new();
    let err = background_jobs_with(
        &mut shell,
        &ctx,
        bg_argv(&["%1", "%2"]),
        &mut |pgid: Pid| {
            seen.push(pgid);
            Ok(())
        },
    )
    .await
    .expect_err("overall status must be non-zero");
    assert!(
        err.to_string().contains("has no process group"),
        "unexpected error: {err}"
    );
    assert_eq!(seen, vec![Pid::from_raw(426032)]);
    // Job 1 stays owned and stopped; job 2 resumed.
    assert_eq!(shell.wait_jobs.len(), 2);
    let first = shell
        .wait_jobs
        .iter()
        .find(|job| job.job_id == 1)
        .expect("job 1 owned");
    assert!(first.has_stopped_process(), "job 1 must stay stopped");
    let second = shell
        .wait_jobs
        .iter()
        .find(|job| job.job_id == 2)
        .expect("job 2 owned");
    assert!(!second.has_stopped_process());
}

#[tokio::test]
async fn bg_running_sibling_failure_still_resumes_stopped_target() {
    use super::bg::background_jobs_with;

    let mut shell = test_shell();
    shell.wait_jobs.push(running_tree_job(1));
    shell.wait_jobs.push(stopped_bg_job(2, 426042));
    let ctx = test_ctx();

    let mut seen = Vec::new();
    let err = background_jobs_with(
        &mut shell,
        &ctx,
        bg_argv(&["%1", "%2"]),
        &mut |pgid: Pid| {
            seen.push(pgid);
            Ok(())
        },
    )
    .await
    .expect_err("overall status must be non-zero");
    assert!(
        err.to_string().contains("already running"),
        "unexpected error: {err}"
    );
    assert_eq!(seen, vec![Pid::from_raw(426042)]);
    assert_eq!(shell.wait_jobs.len(), 2, "both jobs stay owned");
}

#[tokio::test]
async fn bg_duplicate_operand_second_hit_is_already_running() {
    use super::bg::background_jobs_with;

    let mut shell = test_shell();
    shell.wait_jobs.push(stopped_bg_job(2, 426052));
    let ctx = test_ctx();

    let mut seen = Vec::new();
    let err = background_jobs_with(
        &mut shell,
        &ctx,
        bg_argv(&["%2", "%2"]),
        &mut |pgid: Pid| {
            seen.push(pgid);
            Ok(())
        },
    )
    .await
    .expect_err("duplicate operand must not silently dedupe");
    assert!(
        err.to_string().contains("already running"),
        "unexpected error: {err}"
    );
    assert_eq!(seen, vec![Pid::from_raw(426052)], "exactly one SIGCONT");
}

#[test]
fn bg_default_selection_prefers_most_recent_stopped_tree() {
    use super::bg::default_bg_target;

    let mut shell = test_shell();
    shell.wait_jobs.push(stopped_bg_job(1, 426061));
    shell.wait_jobs.push(running_tree_job(2));
    shell.wait_jobs.push(stopped_bg_job(3, 426063));
    assert_eq!(default_bg_target(&shell.wait_jobs), Some(3));

    // Stale `job.state` never decides: tree Stopped wins over the summary.
    let mut stale = stopped_bg_job(4, 426064);
    stale.state = ProcessState::Stopped(Pid::from_raw(9), NixSignal::SIGSTOP);
    let mut shell = test_shell();
    shell.wait_jobs.push(stale);
    assert_eq!(default_bg_target(&shell.wait_jobs), Some(4));
}

#[tokio::test]
async fn bg_no_stopped_job_is_error_not_success() {
    use super::bg::background_jobs_with;

    let ctx = test_ctx();
    let mut send = |_: Pid| Ok(());

    let mut empty = test_shell();
    let err = background_jobs_with(&mut empty, &ctx, bg_argv(&[]), &mut send)
        .await
        .expect_err("empty table must be non-zero");
    assert!(
        !err.to_string().starts_with("bg:"),
        "core error stays prefixless: {err}"
    );

    let mut running = test_shell();
    running.wait_jobs.push(running_tree_job(1));
    background_jobs_with(&mut running, &ctx, bg_argv(&[]), &mut send)
        .await
        .expect_err("no stopped tree must be non-zero");
    assert_eq!(running.wait_jobs.len(), 1, "running job stays owned");
}

#[tokio::test]
async fn bg_sigcont_failure_keeps_active_ownership_through_finalizer() {
    use super::bg::resume_background_job_with;

    let mut shell = test_shell();
    shell.wait_jobs.push(stopped_bg_job(5, 426071));

    let err = resume_background_job_with(&mut shell, 5, &mut |_: Pid| {
        Err(anyhow::anyhow!("SIGCONT failed"))
    })
    .await
    .expect_err("SIGCONT failure must propagate");
    assert!(err.to_string().contains("SIGCONT failed"));

    assert_eq!(shell.wait_jobs.len(), 1, "active job must be requeued");
    assert!(shell.wait_jobs[0].has_stopped_process());
}
