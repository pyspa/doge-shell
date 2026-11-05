//! Deterministic real Job launch on an owned controlling terminal.

use super::{Job, JobProcess, Process, ProcessState};
use crate::{environment::Environment, shell::Shell};
use dsh_types::Context;
use nix::unistd::{Pid, getpgrp, getpid};
use std::cell::Cell;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

thread_local! {
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

pub(super) fn record_child(child: Pid) {
    if SCRATCH_TERMINAL.get() {
        let path = std::env::var_os("DSH_HANDOFF_CHILDREN").unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{child}").unwrap();
    }
}

struct Worker {
    child: std::process::Child,
    children: std::path::PathBuf,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // Kill the worker first, but retain its zombie until the final sweep:
        // its PID/session identity cannot be reused while we validate groups.
        let _ = self.child.kill();
        let worker = self.child.id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let observed = unsafe {
                libc::waitid(
                    libc::P_PID,
                    worker as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            if observed == 0 && unsafe { info.si_pid() } == worker {
                if let Ok(children) = std::fs::read_to_string(&self.children) {
                    for pid in children.lines().filter_map(|pid| pid.parse::<i32>().ok()) {
                        let pid = Pid::from_raw(pid);
                        if nix::unistd::getsid(Some(pid)).ok() == Some(Pid::from_raw(worker))
                            && let Ok(group) = nix::unistd::getpgid(Some(pid))
                            && group.as_raw() > 0
                            && group.as_raw() != worker
                        {
                            let _ =
                                nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
                        }
                    }
                }
                break;
            }
            if observed < 0 || Instant::now() >= deadline {
                break;
            }
            unsafe { libc::poll(std::ptr::null_mut(), 0, 1) };
        }
        let _ = self.child.wait();
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

fn run_scratch(mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let pty = nix::pty::openpty(None, None).unwrap();
    let mut master = std::fs::File::from(pty.master);
    let slave = std::fs::File::from(pty.slave);
    let proof = root.path().join("proof");
    let log = root.path().join("log");
    let children = root.path().join("children");
    let log_file = std::fs::File::create(&log).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "process::foreground_handoff_tests::scratch_session",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("DOGESH_ATUIN_DUAL_WRITE", "0")
        .env("TERM", "dumb")
        .env("DSH_HANDOFF_CASE", mode)
        .env("DSH_HANDOFF_PROOF", &proof)
        .env("DSH_HANDOFF_CHILDREN", &children)
        .current_dir(root.path())
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(log_file.try_clone().unwrap()))
        .stderr(Stdio::from(log_file));
    let controlling = mode != "no-ctty";
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0
                || (controlling && libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut worker = Worker {
        child: command.spawn().unwrap(),
        children,
    };
    master
        .write_all(&b"foreground-ready\n".repeat(if mode == "fd-budget" { 96 } else { 1 }))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let status = loop {
        if let Some(status) = worker.child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        unsafe { libc::poll(std::ptr::null_mut(), 0, 5) };
    };
    drop(worker);
    let output = std::fs::read_to_string(&log).unwrap();
    eprintln!("{output}");
    assert!(
        status.is_some_and(|status| status.success()),
        "{mode}: {status:?}\n{output}"
    );
    if mode == "exec-failure" {
        assert!(output.contains("Permission denied"), "{output}");
    }
    if mode == "handoff-abort" {
        assert!(
            output.contains("failed foreground terminal handoff"),
            "{output}"
        );
    }
    assert_eq!(
        std::fs::read(&proof).unwrap(),
        b"ok",
        "scratch case actually ran"
    );
}

#[test]
fn scratch_session() {
    let Ok(mode) = std::env::var("DSH_HANDOFF_CASE") else {
        return;
    };
    // pre_exec established a fresh session and controlling PTY on fd 0.
    // This permission is thread-local and never set in the cargo test parent.
    let stdin = unsafe { BorrowedFd::borrow_raw(0) };
    assert_eq!(nix::unistd::getsid(None).unwrap(), getpid());
    if mode == "no-ctty" {
        assert!(nix::unistd::isatty(stdin).unwrap());
        assert!(nix::unistd::tcgetpgrp(stdin).is_err());
    } else {
        assert_eq!(nix::unistd::tcgetpgrp(stdin).unwrap(), getpgrp());
        assert_eq!(unsafe { libc::tcgetsid(0) }, getpid().as_raw());
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
                        "read -r line; test \"$line\" = foreground-ready || exit 2; kill -STOP $$"
                            .into()
                    } else {
                        "read -r line; test \"$line\" = foreground-ready".into()
                    },
                ],
            ),
        };
        let mut job = Job::new("terminal reader".into(), shell.pgid);
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
    std::fs::write(std::env::var_os("DSH_HANDOFF_PROOF").unwrap(), b"ok").unwrap();
}
