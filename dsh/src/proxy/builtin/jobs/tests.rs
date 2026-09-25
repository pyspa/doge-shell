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
