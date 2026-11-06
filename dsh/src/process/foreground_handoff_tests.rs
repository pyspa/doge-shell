//! Deterministic real Job launch on an owned controlling terminal.

use super::{Job, JobProcess, Process, ProcessState};
use crate::{environment::Environment, shell::Shell};
use dsh_types::Context;
use nix::unistd::{Pid, getpgrp};
use std::cell::Cell;
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

#[path = "foreground_handoff_tests/pty_pump_tests.rs"]
mod pty_pump;
#[path = "foreground_handoff_tests/supervisor_tests.rs"]
mod supervisor;
use supervisor::run_scratch;

thread_local! {
    static CHILD_START_FD: Cell<i32> = const { Cell::new(-1) };
    static SCRATCH_TERMINAL: Cell<bool> = const { Cell::new(false) };
    static REJECT_HANDOFF: Cell<bool> = const { Cell::new(false) };
    static OBSERVE_FIRST: Cell<bool> = const { Cell::new(false) };
    static FIRST_EVENT: Cell<Option<(i32, i32)>> = const { Cell::new(None) };
}

pub(super) fn scratch_terminal_enabled() -> bool {
    SCRATCH_TERMINAL.get()
}

pub(super) fn reject_handoff() -> bool {
    REJECT_HANDOFF.get()
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
        std::fs::write(
            std::env::var_os("DSH_HANDOFF_FAULT_READY").unwrap(),
            b"forked",
        )
        .unwrap();
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

/// Delay only this parent's existing post-exec/pre-admission boundary. Never
/// consume a status: the real Job waiter still owns every state transition.
pub(super) fn after_exec_handshake(child: Pid) {
    if !OBSERVE_FIRST.replace(false) {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.as_raw() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WSTOPPED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result < 0 {
            FIRST_EVENT.set(Some((
                -1,
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            )));
            return;
        }
        if unsafe { info.si_pid() } == child.as_raw() {
            FIRST_EVENT.set(Some((info.si_code, unsafe { info.si_status() })));
            return;
        }
        if Instant::now() >= deadline {
            FIRST_EVENT.set(Some((-2, 0)));
            return;
        }
        // Bounded observation polling, not sleep-based readiness.
        unsafe { libc::poll(std::ptr::null_mut(), 0, 1) };
    }
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
            "exec-failure" => {
                std::fs::write("denied", "not executable").unwrap();
                ("./denied".to_owned(), vec!["./denied".to_owned()])
            }
            "missing" => (
                "dogesh-missing-handoff-fixture".to_owned(),
                vec!["dogesh-missing-handoff-fixture".to_owned()],
            ),
            _ => (
                "/bin/sh".to_owned(),
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    if mode == "rollback" {
                        format!(": >&{lifetime_fd} || exit 3; read -r line; test \"$line\" = foreground-ready || exit 2; kill -STOP $$")
                    } else {
                        format!(": >&{lifetime_fd} || exit 3; read -r line; test \"$line\" = foreground-ready")
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
        REJECT_HANDOFF.set(mode == "handoff-abort" || mode == "no-ctty");
        OBSERVE_FIRST.set(true);
        FIRST_EVENT.set(None);
        let result = job.launch(&mut ctx, &mut shell).await;
        let first = job.process.as_deref().unwrap().get_state();
        let event = FIRST_EVENT.get();
        eprintln!("case={mode} first_event={event:?} first_state={first:?} outcome={result:?}");
        // Clean up through the canonical owner before assertions even on RED.
        if !job.is_process_tree_completed() {
            job.signal(nix::sys::signal::Signal::SIGKILL).unwrap();
            super::job_wait::wait_job(&mut job, false).await.unwrap();
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
            assert_eq!(event, Some((libc::CLD_STOPPED, libc::SIGSTOP)));
        } else {
            result.unwrap();
            let status = match mode.as_str() {
                "handoff-abort" => 1,
                "exec-failure" => 126,
                "missing" => 127,
                _ => 0,
            };
            assert_eq!(
                event,
                Some((libc::CLD_EXITED, status)),
                "reader must finish before parent admission"
            );
            assert_eq!(first, ProcessState::Completed(status as u8, None));
            assert_eq!(job.final_exit_status(), Some(status));
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
