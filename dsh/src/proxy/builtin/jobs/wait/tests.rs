//! Unit tests for the `wait` builtin: parser, `-p` assignment helpers,
//! and identity-aware completion mapping without spawning real processes.

use super::*;
use crate::process::{JobProcess, Process, ProcessState};

fn interrupt_flag() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
}

/// `wait -n` under SIGINT reports 130, forwards nothing to the
/// background job, and keeps every ownership entry intact. The
/// interrupt arrives as a plain flag (no real signal delivery), so
/// parallel tests can neither steal nor observe it.
#[test]
fn wait_next_interrupt_reports_130_and_keeps_ownership() {
    let flag = interrupt_flag();
    let mut shell = Shell::new(crate::environment::Environment::new());
    let pid = Pid::from_raw(424281);
    let mut job = crate::process::Job::new("sleep 60".to_string(), shell.pgid);
    job.job_id = 1;
    job.pid = Some(pid);
    let mut process = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
    process.pid = Some(pid);
    process.state = ProcessState::Running;
    job.set_process(JobProcess::Command(process));
    shell.wait_jobs.push(job);
    shell.known_async.register(pid, 1);

    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    let probe = flag.clone();
    let outcome = super::super::block_on_job_control_future(wait_next_until(
        &mut shell,
        vec![ResolvedWaitTarget {
            pid,
            job_id: Some(1),
            source: WaitTargetSource::Pid,
        }],
        move || probe.load(std::sync::atomic::Ordering::SeqCst),
    ))
    .expect("bridge executes")
    .expect("wait executes");
    let WaitNextOutcome::Interrupted = outcome else {
        panic!("interrupt must win over a live target");
    };
    assert_eq!(shell.wait_jobs.len(), 1, "interrupted job stays owned");
    assert!(
        shell.known_async.active_entry(pid).is_some(),
        "ledger stays Active"
    );
    assert!(
        shell.known_async.consume_completed(pid).is_none(),
        "nothing consumed"
    );
}

fn parse(argv: &[&str]) -> Result<WaitInvocation> {
    parse_wait_invocation(&argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

fn parsed_ok(argv: &[&str]) -> WaitInvocation {
    parse(argv).expect("parse must succeed")
}

#[test]
fn wait_parse_accepts_p_forms() {
    let invocation = parsed_ok(&["wait", "-p", "DONE", "123"]);
    assert_eq!(invocation.assign_to.as_deref(), Some("DONE"));
    assert!(!invocation.next);
    assert_eq!(invocation.operands, vec!["123".to_string()]);

    let invocation = parsed_ok(&["wait", "-n", "-p", "DONE", "123"]);
    assert_eq!(invocation.assign_to.as_deref(), Some("DONE"));
    assert!(invocation.next);

    let invocation = parsed_ok(&["wait", "-p", "DONE", "-n", "123"]);
    assert_eq!(invocation.assign_to.as_deref(), Some("DONE"));
    assert!(invocation.next);

    let invocation = parsed_ok(&["wait", "-np", "DONE", "123"]);
    assert_eq!(invocation.assign_to.as_deref(), Some("DONE"));
    assert!(invocation.next);

    let invocation = parsed_ok(&["wait", "-n", "-n", "-p", "DONE", "123"]);
    assert_eq!(invocation.assign_to.as_deref(), Some("DONE"));
    assert!(invocation.next);

    let invocation = parsed_ok(&["wait", "--", "123"]);
    assert_eq!(invocation.assign_to, None);
    assert!(!invocation.next);
    assert_eq!(invocation.operands, vec!["123".to_string()]);
}

#[test]
fn wait_parse_repeated_p_last_wins_without_touching_first() {
    let invocation = parsed_ok(&["wait", "-p", "FIRST", "-p", "SECOND", "123"]);
    assert_eq!(invocation.assign_to.as_deref(), Some("SECOND"));
    assert_eq!(invocation.operands, vec!["123".to_string()]);
}

#[test]
fn wait_parse_double_dash_treats_p_as_operand() {
    let invocation = parsed_ok(&["wait", "--", "-p"]);
    assert_eq!(invocation.assign_to, None);
    assert_eq!(invocation.operands, vec!["-p".to_string()]);
}

#[test]
fn wait_parse_rejects_missing_or_invalid_p_and_unknown_options() {
    assert!(parse(&["wait", "-p"]).is_err(), "missing -p value");
    assert!(parse(&["wait", "-p", "123bad"]).is_err());
    assert!(parse(&["wait", "-p", "bad-name"]).is_err());
    assert!(parse(&["wait", "-f"]).is_err());
    assert!(parse(&["wait", "-x"]).is_err());
    assert!(parse(&["wait", "-n", "-p"]).is_err());
}

fn test_shell() -> crate::shell::Shell {
    crate::shell::Shell::new(crate::environment::Environment::new())
}

fn test_ctx(shell: &crate::shell::Shell) -> dsh_types::Context {
    dsh_types::Context::new_safe(shell.pid, shell.pgid, false)
}

#[test]
fn wait_prepare_unsets_value_and_export_bit() {
    let mut shell = test_shell();
    shell
        .environment
        .write()
        .set_and_export_shell_var("DONE".to_string(), "old".to_string());
    prepare_wait_assignment(&mut shell, Some("DONE")).expect("prepare");
    let state = shell.environment.read().shell_var_state("DONE");
    assert_eq!(state.value.as_deref(), None, "value must be gone");
    assert!(!state.exported, "export bit must be gone");
}

#[test]
fn wait_publish_sets_pid_without_export_bit() {
    let mut shell = test_shell();
    shell
        .environment
        .write()
        .set_and_export_shell_var("DONE".to_string(), "old".to_string());
    prepare_wait_assignment(&mut shell, Some("DONE")).expect("prepare");
    publish_wait_assignment(
        &mut shell,
        Some("DONE"),
        Some(WaitCompletion {
            pid: Pid::from_raw(4242),
            job_id: Some(1),
            status: 1,
        }),
    );
    let state = shell.environment.read().shell_var_state("DONE");
    assert_eq!(state.value.as_deref(), Some("4242"));
    assert!(!state.exported, "plain assignment must not re-export");
}

#[test]
fn wait_publish_no_completion_leaves_destination_unset() {
    let mut shell = test_shell();
    shell
        .environment
        .write()
        .set_shell_var("DONE".to_string(), "old".to_string());
    prepare_wait_assignment(&mut shell, Some("DONE")).expect("prepare");
    publish_wait_assignment(&mut shell, Some("DONE"), None);
    let state = shell.environment.read().shell_var_state("DONE");
    assert_eq!(state.value.as_deref(), None);
}

#[test]
fn wait_ledger_completed_127_is_not_unknown_127() {
    let mut shell = test_shell();
    let ctx = test_ctx(&shell);
    let real = Pid::from_raw(424281);
    shell.known_async.register(real, 7);
    assert!(shell.known_async.mark_completed(real, 127));

    let outcome = super::super::block_on_job_control_future(wait_target(
        &mut shell,
        &ctx,
        ResolvedWaitTarget {
            pid: real,
            job_id: None,
            source: WaitTargetSource::Pid,
        },
    ))
    .expect("bridge executes")
    .expect("wait executes");
    match outcome {
        WaitOneOutcome::Completed(completion) => {
            assert_eq!(completion.pid, real);
            assert_eq!(completion.status, 127);
        }
        other => panic!("real child 127 must complete, got {other:?}"),
    }

    let unknown = super::super::block_on_job_control_future(wait_target(
        &mut shell,
        &ctx,
        ResolvedWaitTarget {
            pid: Pid::from_raw(499999),
            job_id: None,
            source: WaitTargetSource::Pid,
        },
    ))
    .expect("bridge executes")
    .expect("wait executes");
    match unknown {
        WaitOneOutcome::NoCompletion(127) => {}
        other => panic!("unknown PID must not complete, got {other:?}"),
    }
}

#[test]
fn wait_sequential_resets_identity_on_trailing_unknown() {
    let mut shell = test_shell();
    let ctx = test_ctx(&shell);
    let real = Pid::from_raw(424282);
    shell.known_async.register(real, 1);
    assert!(shell.known_async.mark_completed(real, 1));

    // Valid completion followed by an unknown target: the final status
    // is 127 with no identity, so `-p` must stay unset.
    let outcome = super::super::block_on_job_control_future(wait_sequential_command(
        &mut shell,
        &ctx,
        &[real.as_raw().to_string(), "999999".to_string()],
    ))
    .expect("bridge executes")
    .expect("wait executes");
    assert_eq!(outcome.status, 127);
    assert_eq!(outcome.completion, None);
}

#[test]
fn wait_sequential_unknown_then_valid_publishes_valid_pid() {
    let mut shell = test_shell();
    let ctx = test_ctx(&shell);
    let real = Pid::from_raw(424283);
    shell.known_async.register(real, 1);
    assert!(shell.known_async.mark_completed(real, 1));

    let outcome = super::super::block_on_job_control_future(wait_sequential_command(
        &mut shell,
        &ctx,
        &["999999".to_string(), real.as_raw().to_string()],
    ))
    .expect("bridge executes")
    .expect("wait executes");
    assert_eq!(outcome.status, 1);
    assert_eq!(
        outcome.completion.map(|completion| completion.pid),
        Some(real)
    );
}

#[test]
fn wait_bare_command_consumes_all_and_publishes_nothing() {
    let mut shell = test_shell();
    let ctx = test_ctx(&shell);
    for (raw, job_id) in [(424284, 1), (424285, 2)] {
        let pid = Pid::from_raw(raw);
        shell.known_async.register(pid, job_id);
        assert!(shell.known_async.mark_completed(pid, 1));
    }
    let outcome = super::super::block_on_job_control_future(wait_bare_command(&mut shell, &ctx))
        .expect("bridge executes")
        .expect("wait executes");
    assert_eq!(
        outcome,
        WaitCommandOutcome {
            status: 0,
            completion: None,
        }
    );
    assert!(shell.known_async.known_pids().is_empty());
}

#[test]
fn wait_next_interrupt_maps_to_130_without_assignment() {
    let flag = interrupt_flag();
    let mut shell = test_shell();
    let pid = Pid::from_raw(424286);
    let mut job = crate::process::Job::new("sleep 60".to_string(), shell.pgid);
    job.job_id = 1;
    job.pid = Some(pid);
    let mut process = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
    process.pid = Some(pid);
    process.state = ProcessState::Running;
    job.set_process(JobProcess::Command(process));
    shell.wait_jobs.push(job);
    shell.known_async.register(pid, 1);
    shell
        .environment
        .write()
        .set_shell_var("DONE".to_string(), "old".to_string());
    prepare_wait_assignment(&mut shell, Some("DONE")).expect("prepare");

    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    let probe = flag.clone();
    let outcome = super::super::block_on_job_control_future(wait_next_until(
        &mut shell,
        vec![ResolvedWaitTarget {
            pid,
            job_id: Some(1),
            source: WaitTargetSource::Pid,
        }],
        move || probe.load(std::sync::atomic::Ordering::SeqCst),
    ))
    .expect("bridge executes")
    .expect("wait executes");
    // Same mapping `wait_next_command` applies: 130 with no identity,
    // so the destination stays unset.
    let mapped = match outcome {
        WaitNextOutcome::Completed(completion) => WaitCommandOutcome {
            status: completion.status,
            completion: Some(completion),
        },
        WaitNextOutcome::Interrupted => WaitCommandOutcome {
            status: 130,
            completion: None,
        },
        WaitNextOutcome::NoTargets => WaitCommandOutcome {
            status: 127,
            completion: None,
        },
    };
    assert_eq!(mapped.status, 130);
    publish_wait_assignment(&mut shell, Some("DONE"), mapped.completion);
    let state = shell.environment.read().shell_var_state("DONE");
    assert_eq!(state.value.as_deref(), None);
}

#[test]
fn wait_next_command_without_targets_reports_127_without_assignment() {
    let mut shell = test_shell();
    let ctx = test_ctx(&shell);
    shell
        .environment
        .write()
        .set_shell_var("DONE".to_string(), "old".to_string());
    prepare_wait_assignment(&mut shell, Some("DONE")).expect("prepare");
    let outcome =
        super::super::block_on_job_control_future(wait_next_command(&mut shell, &ctx, &[]))
            .expect("bridge executes")
            .expect("wait executes");
    assert_eq!(
        outcome,
        WaitCommandOutcome {
            status: 127,
            completion: None,
        }
    );
    publish_wait_assignment(&mut shell, Some("DONE"), outcome.completion);
    let state = shell.environment.read().shell_var_state("DONE");
    assert_eq!(state.value.as_deref(), None);
}
