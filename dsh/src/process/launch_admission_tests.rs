//! Real helper startup acknowledgement and post-startup partial-stop boundary.
use super::launch_admission::{LaunchAdmission, StageAdmission};
use super::preexec_full_proxy_tests::OwnedJob;
use super::{Job, JobProcess, Process, ProcessState};
use nix::sys::signal::{Signal, raise};
use nix::unistd::Pid;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn stop_helper_after_ready() -> nix::Result<()> {
    if std::env::var_os("DSH_TEST_HELPER_STOP_AFTER_READY").is_some() {
        raise(Signal::SIGSTOP)?;
    }
    Ok(())
}

#[test]
fn helper_fixture() {
    let Ok(gate) = std::env::var("DSH_TEST_GATE_FD") else {
        return;
    };
    let ready = std::env::var("DSH_TEST_READY_FD").unwrap();
    super::reexec::await_launch_admission(gate.parse().unwrap(), ready.parse().unwrap()).unwrap();
    // A real ordinary partial stop after consumed-token acknowledgement.
    raise(Signal::SIGSTOP).unwrap();
}

#[test]
fn helper_consumed_ack_ends_startup_stop_propagation() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = OwnedJob(Job::new("helper | sleep".into(), nix::unistd::getpgrp()));
    owner.0.foreground = false; // Never hand off the developer terminal.
    let stage = StageAdmission::for_helper().unwrap();
    let gate = stage.gate_read.as_raw_fd();
    let ready = stage.ready_write.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "process::launch_admission_tests::helper_fixture",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("XDG_DATA_HOME", root.path())
        .env("XDG_STATE_HOME", root.path())
        .env("DOGESH_ATUIN_DUAL_WRITE", "0")
        .env("DSH_TEST_GATE_FD", gate.to_string())
        .env("DSH_TEST_READY_FD", ready.to_string())
        .env("DSH_TEST_HELPER_STOP_AFTER_READY", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            for fd in [gate, ready] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let pid = Pid::from_raw(child.id() as i32);
    let mut process = Process::new("helper fixture".into(), vec![]);
    process.pid = Some(pid);
    owner.0.pid = Some(pid);
    owner.0.pgid = Some(pid);
    owner.0.set_process(JobProcess::Command(process));
    drop(child); // Wait ownership moved to the guarded canonical Job.
    let sleeper = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(pid.as_raw())
        .spawn()
        .unwrap();
    let mut tail = Process::new("sleep".into(), vec![]);
    tail.pid = Some(Pid::from_raw(sleeper.id() as i32));
    owner.0.set_process(JobProcess::Command(tail));
    drop(sleeper);
    let mut admission = LaunchAdmission::default();
    admission.admit(pid, stage);
    owner.0.launch_admission = Some(admission);
    let deadline = Instant::now() + Duration::from_secs(3);
    // Release F before consuming the helper's real stop status. The helper
    // is stopped after R, so its C acknowledgement cannot exist yet.
    while owner.0.has_unreleased_admission() {
        owner.0.progress_launch_admission().unwrap();
        assert!(Instant::now() < deadline, "helper readiness never arrived");
        std::thread::sleep(Duration::from_millis(1));
    }
    while !owner.0.is_fully_stopped() {
        owner.0.update_status();
        assert!(
            Instant::now() < deadline,
            "startup stop did not reach running sibling"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        owner.0.launch_admission.is_some(),
        "unconsumed token must retain startup owner"
    );
    owner.0.signal(Signal::SIGCONT).unwrap(); // Explicit fixture resume.
    owner.0.mark_stopped_processes_running();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        owner.0.update_status();
        if owner.0.launch_admission.is_none() && owner.0.has_stopped_process() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "helper consumed acknowledgement missing"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    owner.0.update_status();
    assert!(
        !owner.0.is_fully_stopped(),
        "post-startup partial stop must not propagate"
    );
    assert_eq!(
        owner
            .0
            .process
            .as_deref()
            .unwrap()
            .next_process()
            .unwrap()
            .get_state(),
        ProcessState::Running
    );
    // OwnedJob kills and canonically reaps both fixtures on success/unwind.
}
