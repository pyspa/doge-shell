//! Continuation delivery and canonical transitions without signalling real groups.
use super::*;
use crate::process::process::Process;

fn pipeline(states: &[ProcessState]) -> Job {
    let mut next = None;
    for state in states.iter().rev() {
        let mut process = Process::new("fixture".into(), vec![]);
        process.state = *state;
        process.next = next;
        next = Some(Box::new(JobProcess::Command(process)));
    }
    let mut job = Job::new("fixture".into(), Pid::from_raw(100));
    job.pgid = Some(Pid::from_raw(200));
    job.set_process(*next.unwrap());
    job.refresh_lifecycle_state();
    job
}

#[test]
fn success_continues_stopped_stages_and_preserves_completed_and_running() {
    let completed = ProcessState::Completed(3, None);
    let stopped = ProcessState::Stopped(Pid::from_raw(200), Signal::SIGSTOP);
    for states in [
        vec![stopped, stopped],
        vec![completed, stopped],
        vec![ProcessState::Running, stopped],
    ] {
        let mut job = pipeline(&states);
        let mut calls = 0;
        resume_job_with(&mut job, |pgid| {
            assert_eq!(pgid.as_raw(), 200);
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert!(!job.has_stopped_process());
        assert_eq!(job.state, ProcessState::Running);
        let mut stage = job.process.clone();
        for before in states {
            let process = stage.take().unwrap();
            let expected = if matches!(before, ProcessState::Stopped(..)) {
                ProcessState::Running
            } else {
                before
            };
            assert_eq!(process.get_state(), expected);
            stage = process.next();
        }
    }
}

#[test]
fn failed_delivery_preserves_every_canonical_state() {
    let states = [
        ProcessState::Completed(3, None),
        ProcessState::Stopped(Pid::from_raw(200), Signal::SIGSTOP),
    ];
    let mut job = pipeline(&states);
    let before = job.process.clone();
    let summary = job.state;
    let error = resume_job_with(&mut job, |_| Err(anyhow::anyhow!("injected ESRCH"))).unwrap_err();
    assert!(error.to_string().contains("ESRCH"));
    assert_eq!(job.process, before);
    assert_eq!(job.state, summary);
}

#[test]
fn missing_invalid_and_shell_groups_fail_before_delivery() {
    for pgid in [
        None,
        Some(Pid::from_raw(0)),
        Some(Pid::from_raw(-1)),
        Some(Pid::from_raw(100)),
        Some(nix::unistd::getpgrp()),
    ] {
        let mut job = pipeline(&[ProcessState::Stopped(Pid::from_raw(200), Signal::SIGSTOP)]);
        job.pgid = pgid;
        let before = job.process.clone();
        assert!(
            resume_job_with(&mut job, |_| panic!("unsafe group must never be signalled")).is_err()
        );
        assert_eq!(job.process, before);
    }
}

#[tokio::test]
async fn foreground_resume_without_terminal_continues_owned_group() {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    assert!(
        !owns_terminal(),
        "unit test must never hand off a developer terminal"
    );
    let mut child = OwnedChild(
        Command::new("sh")
            .args(["-c", "kill -STOP $$; exit 7"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let pid = Pid::from_raw(child.0.id() as i32);
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG))
                .unwrap()
            {
                nix::sys::wait::WaitStatus::Stopped(_, signal) => break signal,
                nix::sys::wait::WaitStatus::StillAlive => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                other => panic!("unexpected child state: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    let mut job = pipeline(&[ProcessState::Stopped(pid, observed)]);
    job.pgid = Some(pid);
    if let Some(process) = job.process.as_mut()
        && let JobProcess::Command(process) = &mut **process
    {
        process.pid = Some(pid);
    }
    tokio::time::timeout(
        Duration::from_secs(3),
        put_in_foreground(&mut job, true, true),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(job.last_process_state(), ProcessState::Completed(7, None));
    let _ = child.0.wait();
}
