//! Deterministic real Job launch on an owned controlling terminal.

use super::{Job, JobProcess, Process, ProcessState};
use crate::{environment::Environment, shell::Shell};
use dsh_types::Context;
use nix::unistd::{Pid, getpgrp};
use std::cell::{Cell, RefCell};
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

#[path = "foreground_handoff_tests/pty_pump_tests.rs"]
mod pty_pump;
#[path = "foreground_handoff_tests/supervisor_tests.rs"]
mod supervisor;
use supervisor::run_scratch;

#[derive(Debug)]
enum LaunchObservation {
    Wait(Pid, ProcessState),
    StartupStop,
}

thread_local! {
    static LAUNCH_OBSERVATIONS: RefCell<Vec<LaunchObservation>> = const { RefCell::new(Vec::new()) };
    static ADMISSION_CHECKED: Cell<bool> = const { Cell::new(false) };
    static PREEXEC_BEFORE_ACK: Cell<bool> = const { Cell::new(false) };
    static PREEXEC_STAGE: Cell<usize> = const { Cell::new(0) };
    static PREEXEC_FDS: Cell<(i32, i32)> = const { Cell::new((-1, -1)) };
    static CHILD_START_FD: Cell<i32> = const { Cell::new(-1) };
    static SCRATCH_TERMINAL: Cell<bool> = const { Cell::new(false) };
    static REJECT_HANDOFF: Cell<bool> = const { Cell::new(false) };
}

/// Record the existing known-PID wait result, without waiting or changing it.
/// This runs in the parent, never in the raw fork child.
pub(super) fn record_launch_wait(pid: Pid, state: ProcessState) {
    if SCRATCH_TERMINAL.get() {
        LAUNCH_OBSERVATIONS
            .with_borrow_mut(|events| events.push(LaunchObservation::Wait(pid, state)));
    }
}

/// Called only after the actual startup SIGSTOP propagation operation.
pub(super) fn record_startup_stop() {
    if SCRATCH_TERMINAL.get() {
        LAUNCH_OBSERVATIONS.with_borrow_mut(|events| events.push(LaunchObservation::StartupStop));
    }
}

fn assert_launch_stop_evidence(job: &Job, trigger: nix::sys::signal::Signal) {
    use nix::sys::signal::Signal;
    LAUNCH_OBSERVATIONS.with_borrow(|events| {
        eprintln!("canonical launch observations: {events:?}");
        let mut first_stop = None;
        let mut propagated = false;
        let mut observed = std::collections::HashMap::new();
        for event in events {
            match *event {
                LaunchObservation::Wait(pid, state) => {
                    if let ProcessState::Stopped(stopped_pid, signal) = state {
                        assert_eq!(pid, stopped_pid);
                        if first_stop.is_none() {
                            assert_eq!(
                                signal, trigger,
                                "first real stop must match the injected signal"
                            );
                            first_stop = Some(state);
                        }
                        // A terminal Ctrl-Z starts with TSTP; later STOP is
                        // valid only after recorded startup propagation.
                        assert!(
                            signal == trigger || (signal == Signal::SIGSTOP && propagated),
                            "unexplained stop signal: {state:?}, events={events:?}"
                        );
                    }
                    observed.insert(pid, state);
                }
                LaunchObservation::StartupStop => {
                    assert!(
                        first_stop.is_some(),
                        "propagation must follow a real stop observation"
                    );
                    propagated = true;
                }
            }
        }
        assert!(first_stop.is_some(), "no actual stop observed");
        let mut stage = job.process.as_deref();
        let mut first_stopped = None;
        while let Some(process) = stage {
            let pid = process.get_pid().unwrap();
            let state = process.get_state();
            assert_eq!(
                observed.get(&pid),
                Some(&state),
                "canonical state differs from real wait for {pid}"
            );
            match state {
                ProcessState::Stopped(..) => {
                    first_stopped.get_or_insert(state);
                }
                ProcessState::Completed(..) => {} // An already-exited leader is neutral.
                ProcessState::Running => panic!("live stage {pid} did not stop"),
            }
            stage = process.next_process();
        }
        assert_eq!(
            Some(job.lifecycle_state()),
            first_stopped,
            "summary must be the first real stopped stage"
        );
        assert!(job.is_fully_stopped());
        assert_eq!(job.final_exit_status(), None);
    });
}

/// Pure oracle controls: the CI ordering (middle TSTP, propagation, head
/// STOP) is valid, but missing cause/propagation or a fabricated summary is
/// not. These fixtures do not send signals or stand in for the real PTY test.
#[test]
fn stop_oracle_requires_real_trigger_propagation_and_matching_summary() {
    use nix::sys::signal::Signal::{SIGSTOP, SIGTSTP, SIGTTIN};
    let head = Pid::from_raw(11);
    let middle = Pid::from_raw(12);
    let stopped_head = ProcessState::Stopped(head, SIGSTOP);
    let stopped_middle = ProcessState::Stopped(middle, SIGTSTP);
    let mut job = Job::new("oracle fixture".into(), getpgrp());
    for (pid, state) in [(head, stopped_head), (middle, stopped_middle)] {
        let mut process = Process::new("fixture".into(), vec![]);
        process.pid = Some(pid);
        process.state = state;
        job.set_process(JobProcess::Command(process));
    }
    job.refresh_lifecycle_state();
    let valid = || {
        vec![
            LaunchObservation::Wait(middle, stopped_middle),
            LaunchObservation::StartupStop,
            LaunchObservation::Wait(head, stopped_head),
        ]
    };
    LAUNCH_OBSERVATIONS.set(valid());
    assert_launch_stop_evidence(&job, SIGTSTP);
    for events in [
        vec![
            LaunchObservation::StartupStop,
            LaunchObservation::Wait(head, stopped_head),
        ],
        vec![
            LaunchObservation::Wait(middle, stopped_middle),
            LaunchObservation::Wait(head, stopped_head),
        ],
        vec![LaunchObservation::Wait(
            middle,
            ProcessState::Stopped(middle, SIGTTIN),
        )],
    ] {
        LAUNCH_OBSERVATIONS.set(events);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_launch_stop_evidence(&job, SIGTSTP);
            }))
            .is_err()
        );
    }
    LAUNCH_OBSERVATIONS.set(valid());
    job.set_lifecycle_state_for_test(stopped_middle); // Wrong summary despite a valid observed tree.
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_launch_stop_evidence(&job, SIGTSTP);
        }))
        .is_err()
    );
}

pub(super) fn admission_ready_checkpoint() {
    if !SCRATCH_TERMINAL.get() {
        return;
    }
    let mode = std::env::var("DSH_HANDOFF_CASE").unwrap();
    if mode == "normal" || mode == "handoff-abort" {
        assert!(
            !std::path::Path::new("exec-started").exists(),
            "ready child crossed unreleased gate"
        );
        assert_eq!(
            nix::unistd::tcgetpgrp(unsafe { BorrowedFd::borrow_raw(0) }).unwrap(),
            getpgrp()
        );
        ADMISSION_CHECKED.set(true);
        eprintln!(
            "all stage readiness acknowledged; terminal still shell-owned; exec marker absent"
        );
    }
}

pub(super) fn scratch_terminal_enabled() -> bool {
    SCRATCH_TERMINAL.get()
}

pub(super) fn reject_handoff() -> bool {
    REJECT_HANDOFF.get()
}

pub(super) fn preexec_before_ack() -> bool {
    PREEXEC_BEFORE_ACK.get()
}

pub(super) fn preexec_fds() -> (i32, i32) {
    let stage = PREEXEC_STAGE.get();
    if stage == 0 {
        PREEXEC_FDS.replace((-1, -1))
    } else {
        PREEXEC_STAGE.set(stage - 1);
        (-1, -1)
    }
}

/// Raw-only child seam after signal defaults, before exec.
/// # Safety
/// Descriptors are prepared by the scratch supervisor before fork.
pub(super) unsafe fn hold_before_exec(ready: i32, hold: i32) {
    if ready < 0 {
        return;
    }
    unsafe {
        let pid = libc::getpid();
        if libc::write(
            ready,
            (&pid as *const libc::pid_t).cast(),
            std::mem::size_of::<libc::pid_t>(),
        ) != std::mem::size_of::<libc::pid_t>() as isize
        {
            libc::_exit(98);
        }
        libc::close(ready);
        let mut byte = 0u8;
        loop {
            let result = libc::read(hold, (&mut byte as *mut u8).cast(), 1);
            if result == 0 {
                break;
            }
            if result < 0 {
                libc::_exit(98);
            }
        }
        libc::close(hold);
    }
}

#[test]
fn preexec_direct_stop_after_release_stops_pipeline() {
    for stage in ["first", "middle", "tail"] {
        run_scratch(&format!("preexec-direct-{stage}"));
    }
}

#[test]
fn preexec_ctrl_z_returns_stopped_job() {
    run_scratch("preexec-stop");
}
#[test]
fn preexec_ctrl_z_stops_complete_pipeline() {
    run_scratch("preexec-leader");
    for stage in ["first", "middle", "tail"] {
        run_scratch(&format!("preexec-pipeline-{stage}"));
    }
}

pub(super) fn child_start_gate_fd() -> i32 {
    CHILD_START_FD.get()
}

/// Test-only child seam: raw read/close only, before actual setpgid/exec.
/// Parent prepared fd; the supervisor keeps its writer through cleanup.
///
/// # Safety
/// Called only immediately after fork, with a parent-prepared valid reader
/// or -1. No Rust runtime/allocator/lock is used on this child path.
pub(super) unsafe fn hold_before_setpgid(fd: i32) {
    if fd >= 0 {
        let mut byte = 0u8;
        unsafe {
            let count = libc::read(fd, (&mut byte as *mut u8).cast(), 1);
            libc::close(fd);
            if count != 1 {
                libc::_exit(98);
            }
        }
    }
}

pub(super) fn record_child(_child: Pid) {
    if !SCRATCH_TERMINAL.get() {
        return;
    }
    let mode = std::env::var("DSH_HANDOFF_CASE").unwrap();
    if mode.starts_with("worker-") || mode == "supervisor-panic" {
        let ready = std::path::PathBuf::from(std::env::var_os("DSH_HANDOFF_FAULT_READY").unwrap());
        let pending = ready.with_extension("pending");
        // Existence is the supervisor's readiness signal: publish only
        // after the complete payload is written, never an empty new file.
        std::fs::write(&pending, b"forked").unwrap();
        std::fs::rename(pending, ready).unwrap();
        match mode.as_str() {
            "worker-panic" => panic!("injected panic after real external fork before admission"),
            "worker-abort" => std::process::abort(),
            "worker-timeout" | "worker-parent-unwind" | "supervisor-panic" => loop {
                unsafe { libc::poll(std::ptr::null_mut(), 0, 100) };
            },
            _ => unreachable!(),
        }
    }
}

#[test]
fn preexec_external_cont_reconciles_without_terminal_handoff() {
    for suffix in ["complete", "read", "helper"] {
        run_scratch(&format!("preexec-admission-external-cont-{suffix}"));
    }
}

#[test]
fn preexec_admission_stop_resumes_fg_and_bg() {
    run_scratch("preexec-admission-fg");
    run_scratch("preexec-admission-bg");
    run_scratch("preexec-admission-bg-read");
    run_scratch("preexec-admission-helper");
    run_scratch("preexec-detach-stop");
}
#[test]
fn preexec_resume_preserves_failure_status_and_repeated_stop() {
    run_scratch("preexec-denied");
    run_scratch("preexec-missing");
    run_scratch("preexec-repeat");
    run_scratch("preexec-denied-redirect");
    run_scratch("preexec-int");
}

#[test]
fn foreground_reader_launch_has_terminal_before_exec() {
    run_scratch("normal");
}

#[test]
fn foreground_pipeline_reader_launch_has_terminal_before_exec() {
    run_scratch("output-only-pipeline");
}

#[test]
fn foreground_gate_abort_preserves_admission_and_reaping() {
    run_scratch("handoff-abort");
}

#[test]
fn foreground_handoff_restores_terminal_after_downstream_failure() {
    run_scratch("rollback");
}

#[test]
fn foreground_handoff_preserves_exec_failure_diagnostic() {
    run_scratch("exec-failure");
    run_scratch("missing");
}

#[test]
fn foreground_reader_without_controlling_terminal_keeps_existing_behavior() {
    run_scratch("no-ctty");
}

#[test]
fn foreground_gate_repeated_launch_respects_fd_budget() {
    run_scratch("fd-budget");
}

#[test]
fn supervisor_cleans_external_children_after_worker_panic() {
    run_scratch("worker-panic");
}

#[test]
fn supervisor_cleans_external_children_after_worker_abort() {
    run_scratch("worker-abort");
}

#[test]
fn supervisor_cleans_external_children_after_worker_timeout() {
    run_scratch("worker-timeout");
}

#[test]
fn supervisor_unwind_runs_the_same_owned_group_cleanup() {
    run_scratch("supervisor-panic");
}

#[test]
fn parent_unwind_requests_supervisor_owned_cleanup() {
    run_scratch("worker-parent-unwind");
}

#[test]
fn scratch_session() {
    if std::env::var("DSH_HANDOFF_ROLE").as_deref() != Ok("worker") {
        return;
    }
    let Ok(mode) = std::env::var("DSH_HANDOFF_CASE") else {
        return;
    };
    // The supervisor owns this scratch session; the worker has its own
    // anchored shell group, distinct from the anchored external job group.
    // This permission is thread-local and never set in the cargo test parent.
    let stdin = unsafe { BorrowedFd::borrow_raw(0) };
    let session = Pid::from_raw(
        std::env::var("DSH_HANDOFF_SUPERVISOR")
            .unwrap()
            .parse()
            .unwrap(),
    );
    let job_group = Pid::from_raw(
        std::env::var("DSH_HANDOFF_JOB_GROUP")
            .unwrap()
            .parse()
            .unwrap(),
    );
    assert_eq!(nix::unistd::getsid(None).unwrap(), session);
    assert_ne!(getpgrp(), job_group);
    if mode == "no-ctty" {
        assert!(nix::unistd::isatty(stdin).unwrap());
        assert!(nix::unistd::tcgetpgrp(stdin).is_err());
    } else {
        assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), getpgrp());
        assert_eq!(unsafe { libc::tcgetsid(0) }, session.as_raw());
    }
    if mode == "fd-budget" {
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit.rlim_cur = limit.rlim_cur.min(64);
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    }
    let core_limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_CORE, &core_limit) },
        0
    );
    let lifetime_fd: i32 = std::env::var("DSH_HANDOFF_LIFETIME_FD")
        .unwrap()
        .parse()
        .unwrap();
    let lifetime_flags = unsafe { libc::fcntl(lifetime_fd, libc::F_GETFD) };
    assert!(lifetime_flags >= 0 && lifetime_flags & libc::FD_CLOEXEC == 0);
    CHILD_START_FD.set(
        std::env::var("DSH_HANDOFF_CHILD_START_FD")
            .unwrap()
            .parse()
            .unwrap(),
    );
    if mode.starts_with("preexec-") {
        PREEXEC_BEFORE_ACK.set(mode.starts_with("preexec-admission"));
        PREEXEC_STAGE.set(if mode.ends_with("middle") || mode == "preexec-leader" {
            1
        } else if mode.ends_with("tail") {
            2
        } else {
            0
        });
        PREEXEC_FDS.set((
            std::env::var("DSH_PREEXEC_READY_FD")
                .unwrap()
                .parse()
                .unwrap(),
            std::env::var("DSH_PREEXEC_HOLD_FD")
                .unwrap()
                .parse()
                .unwrap(),
        ));
    }
    SCRATCH_TERMINAL.set(true);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut shell = Shell::new(Environment::new());
        shell.set_signals();
        for _ in 0..if mode == "fd-budget" { 96 } else { 1 } {
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, true);
        ctx.interactive = true;
        ctx.save_history = false;
        ctx.pgid = Some(job_group);
        let entry_modes = nix::sys::termios::tcgetattr(stdin).unwrap();
        let entry = (
            ctx.infile,
            ctx.outfile,
            ctx.errfile,
            ctx.pgid,
            ctx.process_count,
        );
        let (program, args) = match mode.as_str() {
            "exec-failure" | "preexec-denied" | "preexec-denied-redirect" => {
                std::fs::write("denied", "not executable").unwrap();
                ("./denied".to_owned(), vec!["./denied".to_owned()])
            }
            "missing" | "preexec-missing" => (
                "dogesh-missing-handoff-fixture".to_owned(),
                vec!["dogesh-missing-handoff-fixture".to_owned()],
            ),
            _ => (
                "/bin/sh".to_owned(),
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    if mode == "rollback" {
                        format!(": > exec-started; : >&{lifetime_fd} || exit 3; read -r line; test \"$line\" = foreground-ready || exit 2; kill -STOP $$")
                    } else if mode == "preexec-admission-bg" || mode == "preexec-leader" || (mode.starts_with("preexec-admission-external-cont") && !mode.ends_with("-read")) {
                        "exit 0".to_string()
                    } else if mode == "preexec-repeat" {
                        "kill -STOP $$; exit 0".to_string()
                    } else {
                        format!(": > exec-started; : >&{lifetime_fd} || exit 3; read -r line; test \"$line\" = foreground-ready")
                    },
                ],
            ),
        };
        // Existing-group launch is deliberate: the supervisor's live leader
        // owns group identity through cleanup, even if this worker aborts.
        ctx.pgid = Some(job_group);
        let mut job = Job::new("terminal reader".into(), shell.pgid);
        job.pgid = Some(job_group);
        job.set_process(JobProcess::Command(Process::new(program, args)));
        job.disable_pty = mode != "output-only-pipeline";
        if mode == "output-only-pipeline" {
            job.set_process(JobProcess::Command(Process::new(
                "true".into(),
                vec!["true".into()],
            )));
        } else if mode == "rollback" {
            job.set_process(JobProcess::Command(Process::new(
                "true".into(),
                vec!["true".into(), "bad\0arg".into()],
            )));
        }
        if mode.starts_with("preexec-pipeline") || mode.starts_with("preexec-direct") {
            for _ in 0..2 { job.set_process(JobProcess::Command(Process::new("cat".into(), vec!["cat".into()]))); }
        }
        if mode == "preexec-denied-redirect"
            && let Some(JobProcess::Command(process)) = job.process.as_deref_mut() {
            process.redirects.push(super::Redirect::write(2, "diagnostic".into()));
        }
        if mode.contains("helper") {
            let handler = dsh_builtin::get_handler("set").unwrap();
            job.set_process(JobProcess::Builtin(super::builtin::BuiltinProcess::new_handler("set".into(), handler, vec!["set".into(), "-o".into(), "pipefail".into()])));
        }
        if mode == "preexec-leader" {
            let command = job.process.take().unwrap();
            job.set_process(JobProcess::Command(Process::new("true".into(), vec!["true".into()])));
            job.set_process(*command);
            job.set_process(JobProcess::Command(Process::new("cat".into(), vec!["cat".into()])));
        }
        REJECT_HANDOFF.set(mode == "handoff-abort" || mode == "no-ctty");
        let result = job.launch(&mut ctx, &mut shell).await;
        let first = job.process.as_deref().unwrap().get_state();
        eprintln!("case={mode} first_state={first:?} outcome={result:?}");
        if mode == "preexec-int" {
            assert_eq!(job.final_exit_status(), Some(128 + libc::SIGINT));
        } else if mode.starts_with("preexec-") {
            assert!(job.is_fully_stopped(), "all admitted stages must stop: {:?}", job.process);
            assert_eq!(job.final_exit_status(), None);
            let stop_signal = if mode.starts_with("preexec-admission") || mode.starts_with("preexec-direct") { nix::sys::signal::Signal::SIGSTOP } else { nix::sys::signal::Signal::SIGTSTP };
            assert_launch_stop_evidence(&job, stop_signal);
            assert!(matches!(&result, Ok(super::launch_outcome::JobLaunchOutcome::Process(state)) if *state == job.lifecycle_state()),
                "launch outcome must preserve the canonical summary: {result:?}");
            assert_eq!(job.has_unreleased_admission(), mode.starts_with("preexec-admission"));
            assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), shell.pgid);
            if mode == "preexec-detach-stop" {
                job.foreground = false;
                shell.track_async_job(job).unwrap();
                assert!(shell.detach_known_async_jobs_for_normal_exit().is_err());
                assert_eq!(shell.wait_jobs.len(), 1);
                job = shell.wait_jobs.pop().unwrap();
                assert!(!job.exec_reports.is_empty());
            }
            std::fs::write("launch-returned", b"stopped").unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !std::path::Path::new("preexec-released").exists() {
                assert!(Instant::now() < deadline, "supervisor did not release test hold");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if mode.starts_with("preexec-admission-external-cont") {
                // Simulate an external actor: no fg/bg or local state mutation.
                nix::sys::signal::killpg(job_group, nix::sys::signal::Signal::SIGCONT).unwrap();
                eprintln!("external group SIGCONT only; reconciling real stopped Job");
                let deadline = Instant::now() + Duration::from_secs(2);
                shell.wait_jobs.push(job);
                job = loop {
                    let mut completed = shell.check_job_state().await.unwrap();
                    assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), shell.pgid,
                        "external continuation must not take the shell terminal");
                    if let Some(done) = completed.pop() {
                        assert!(completed.is_empty() && shell.wait_jobs.is_empty());
                        break done;
                    }
                    let active = shell.wait_jobs.last().unwrap();
                    if matches!(active.lifecycle_state(), ProcessState::Stopped(_, nix::sys::signal::Signal::SIGTTIN)) {
                        break shell.wait_jobs.pop().unwrap();
                    }
                    assert!(Instant::now() < deadline,
                        "external continuation did not progress admission: {:?}", active.lifecycle_state());
                    tokio::time::sleep(Duration::from_millis(1)).await;
                };
                if mode.ends_with("-read") {
                    assert!(matches!(job.lifecycle_state(), ProcessState::Stopped(_, nix::sys::signal::Signal::SIGTTIN)));
                    assert_eq!(job.final_exit_status(), None);
                    super::job_wait::put_in_foreground(&mut job, false, true).await.unwrap();
                }
            } else if mode.starts_with("preexec-admission-bg") {
                job.signal(nix::sys::signal::Signal::SIGCONT).unwrap();
                crate::proxy::finalize_background_resume(&mut shell, job, Ok(())).await.unwrap();
                job = shell.wait_jobs.pop().unwrap();
                if mode.ends_with("-read") {
                    super::job_wait::wait_job(&mut job, false).await.unwrap();
                    assert!(matches!(job.lifecycle_state(), ProcessState::Stopped(_, nix::sys::signal::Signal::SIGTTIN)));
                    assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), shell.pgid);
                    super::job_wait::put_in_foreground(&mut job, false, true).await.unwrap();
                } else {
                    super::job_wait::wait_for_termination(&mut job).await.unwrap();
                }
                assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), shell.pgid);
            } else {
                super::job_wait::put_in_foreground(&mut job, false, true).await.unwrap();
                if mode == "preexec-repeat" {
                    assert!(matches!(job.lifecycle_state(), ProcessState::Stopped(_, nix::sys::signal::Signal::SIGSTOP)));
                    assert_eq!(job.final_exit_status(), None);
                    super::job_wait::put_in_foreground(&mut job, false, true).await.unwrap();
                }
            }
            let status = match mode.as_str() { "preexec-denied" | "preexec-denied-redirect" => 126, "preexec-missing" => 127, _ => 0 };
            assert_eq!(job.final_exit_status(), Some(status));
            assert!(job.exec_reports.is_empty());
            if mode == "preexec-denied-redirect" {
                let diagnostic = std::fs::read_to_string("diagnostic").unwrap();
                assert_eq!(diagnostic.lines().filter(|line| line.contains("Permission denied")).count(), 1);
            }
        }
        // Clean up through the canonical owner before assertions even on RED.
        if !job.is_process_tree_completed() {
            job.signal(nix::sys::signal::Signal::SIGKILL).unwrap();
            super::job_wait::wait_for_termination(&mut job).await.unwrap();
            job.cleanup_pty_tasks().await;
        }
        if mode == "no-ctty" {
            assert!(nix::unistd::tcgetpgrp(stdin).is_err());
        } else {
            assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), shell.pgid);
        }
        assert_eq!(nix::sys::termios::tcgetattr(stdin).unwrap(), entry_modes);
        assert_eq!(
            (
                ctx.infile,
                ctx.outfile,
                ctx.errfile,
                ctx.pgid,
                ctx.process_count
            ),
            entry
        );
        if mode == "rollback" {
            assert!(result.unwrap_err().to_string().contains("CString"));
        } else if mode.starts_with("preexec-") {
            result.unwrap();
        } else {
            result.unwrap();
            let status = match mode.as_str() {
                "handoff-abort" => 1,
                "exec-failure" => 126,
                "missing" => 127,
                _ => 0,
            };
            assert_eq!(first, ProcessState::Completed(status as u8, None));
            assert_eq!(job.final_exit_status(), Some(status));
        }
        if mode == "normal" || mode == "handoff-abort" {
            assert!(ADMISSION_CHECKED.get(), "new admission fence must be exercised");
            assert_eq!(std::path::Path::new("exec-started").exists(), mode == "normal");
        }
        assert!(job.is_process_tree_completed());
        let mut stage = job.process.as_deref();
        while let Some(process) = stage {
            let pid = process.get_pid().unwrap();
            assert_eq!(
                nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)),
                Err(nix::errno::Errno::ECHILD)
            );
            stage = process.next_process();
        }
        assert!(job.pty.is_none());
        assert!(job.pty_input_task.is_none());
        assert!(job.pty_output_task.is_none());
        }
    });
    SCRATCH_TERMINAL.set(false);
    std::fs::write(std::env::var_os("DSH_HANDOFF_WORKER_PROOF").unwrap(), b"ok").unwrap();
}
