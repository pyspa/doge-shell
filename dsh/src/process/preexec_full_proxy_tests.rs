//! FullProxy startup can stop before its first setsid, with canonical ownership.
use super::{Job, JobProcess, Process, ProcessState};
use crate::{environment::Environment, shell::Shell};
use dsh_types::Context;
use nix::sys::signal::Signal;
use std::cell::Cell;
use std::time::{Duration, Instant};
thread_local! { static STOP: Cell<bool> = const { Cell::new(false) }; }
pub(super) fn take_stop() -> bool {
    STOP.replace(false)
}

pub(super) struct OwnedJob(pub(super) Job);
impl Drop for OwnedJob {
    fn drop(&mut self) {
        if self.0.is_process_tree_completed() {
            return;
        }
        let _ = self.0.signal(Signal::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.0.is_process_tree_completed() && Instant::now() < deadline {
            self.0.update_status();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            self.0.is_process_tree_completed(),
            "owned FullProxy fixture was not reaped"
        );
    }
}

#[tokio::test]
async fn full_proxy_pre_setsid_stop_resumes_foreground() {
    check_resume(false).await;
}
#[tokio::test]
async fn full_proxy_pre_setsid_stop_resumes_background() {
    check_resume(true).await;
}
async fn check_resume(background: bool) {
    let mut shell = Shell::new(Environment::new());
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, true);
    ctx.interactive = true;
    ctx.save_history = false;
    let mut owner = OwnedJob(Job::new("true".into(), shell.pgid));
    owner.0.set_process(JobProcess::Command(Process::new(
        "true".into(),
        vec!["true".into()],
    )));
    STOP.set(true);
    tokio::time::timeout(Duration::from_secs(3), owner.0.launch(&mut ctx, &mut shell))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.0.pty_mode, Some(super::pty::PtyMode::FullProxy));
    assert!(matches!(
        owner.0.lifecycle_state(),
        ProcessState::Stopped(_, Signal::SIGSTOP)
    ));
    let pid = owner.0.pid.unwrap();
    assert_ne!(
        nix::unistd::getpgid(Some(pid)).unwrap(),
        pid,
        "fixture must stop before setsid"
    );
    assert_eq!(
        super::job_wait::pending_full_proxy_stopped_pid(&owner.0),
        Some(pid)
    );
    let report = owner.0.exec_reports.pop().unwrap();
    assert_eq!(
        super::job_wait::pending_full_proxy_stopped_pid(&owner.0),
        None
    );
    owner.0.exec_reports.push(report);
    owner.0.exec_reports[0].pid = Some(nix::unistd::Pid::from_raw(pid.as_raw() + 1));
    let mismatched_report = super::job_wait::pending_full_proxy_stopped_pid(&owner.0);
    owner.0.exec_reports[0].pid = Some(pid);
    assert_eq!(mismatched_report, None);
    let mode = owner.0.pty_mode.take();
    let without_full_proxy = super::job_wait::pending_full_proxy_stopped_pid(&owner.0);
    owner.0.pty_mode = mode;
    assert_eq!(without_full_proxy, None);
    owner.0.pid = Some(nix::unistd::Pid::from_raw(pid.as_raw() + 1));
    let mismatch = super::job_wait::pending_full_proxy_stopped_pid(&owner.0);
    owner.0.pid = Some(pid);
    assert_eq!(mismatch, None);
    for state in [ProcessState::Running, ProcessState::Completed(0, None)] {
        let old = match owner.0.process.as_deref_mut().unwrap() {
            JobProcess::Command(process) => std::mem::replace(&mut process.state, state),
            _ => unreachable!(),
        };
        let candidate = super::job_wait::pending_full_proxy_stopped_pid(&owner.0);
        if let JobProcess::Command(process) = owner.0.process.as_deref_mut().unwrap() {
            process.state = old;
        }
        assert_eq!(candidate, None);
    }
    owner.0.set_process(JobProcess::Command(Process::new(
        "policy-only-tail".into(),
        vec![],
    )));
    let multi = super::job_wait::pending_full_proxy_stopped_pid(&owner.0);
    owner.0.process.as_deref_mut().unwrap().take_next();
    assert_eq!(multi, None);
    assert!(
        super::job_wait::continue_startup_fallback(Some(pid), Err(nix::errno::Errno::EPERM.into()))
            .is_err()
    );
    assert!(
        super::job_wait::continue_startup_fallback(None, Err(nix::errno::Errno::ESRCH.into()))
            .is_err()
    );
    if background {
        let job = std::mem::replace(&mut owner.0, Job::new("placeholder".into(), shell.pgid));
        let id = job.job_id;
        shell.wait_jobs.push(job);
        let result = crate::proxy::resume_background_job_with(&mut shell, id, &mut |pgid| {
            nix::sys::signal::killpg(pgid, Signal::SIGCONT).map_err(Into::into)
        })
        .await;
        owner.0 = shell.wait_jobs.pop().unwrap();
        result.unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            super::job_wait::wait_for_termination(&mut owner.0),
        )
        .await
        .unwrap()
        .unwrap();
    } else {
        let resumed = tokio::time::timeout(
            Duration::from_secs(3),
            super::job_wait::put_in_foreground(&mut owner.0, false, true),
        )
        .await
        .unwrap();
        resumed.unwrap();
    }
    assert_eq!(owner.0.final_exit_status(), Some(0));
    assert!(owner.0.exec_reports.is_empty());
    owner.0.cleanup_pty_tasks().await;
}
