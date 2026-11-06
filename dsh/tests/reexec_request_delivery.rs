//! A stopped helper must not block its parent while receiving a large request.
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt as _;

#[test]
fn delivery_probe() {
    if std::env::var_os("DOGESH_DELIVERY_PROBE").is_none() {
        return;
    }
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .unwrap();
    let payload = vec![b' '; 2 * 1024 * 1024];
    let result = doge_shell::process::reexec::spawn_internal_helper(
        null.as_raw_fd(),
        null.as_raw_fd(),
        null.as_raw_fd(),
        &payload,
        nix::unistd::Pid::from_raw(0),
        None,
        doge_shell::process::reexec::HelperSpawnSignalPolicy::Normal,
    );
    let pid = result.expect("large request must be delivered independently of the reader");
    struct HelperGuard(nix::unistd::Pid);
    impl Drop for HelperGuard {
        fn drop(&mut self) {
            let _ = nix::sys::signal::kill(self.0, nix::sys::signal::Signal::SIGKILL);
            let _ = nix::sys::wait::waitpid(self.0, None);
        }
    }
    // Every assertion below still kills/reaps exactly our helper on failure.
    let _helper = HelperGuard(pid);
    assert!(matches!(
        nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WUNTRACED)).unwrap(),
        nix::sys::wait::WaitStatus::Stopped(_, nix::sys::signal::Signal::SIGSTOP)
    ));
}

#[test]
fn large_request_delivery_does_not_wait_for_a_stopped_reader() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = dir.path().join("helper");
    let mut file = std::fs::File::create(&script).unwrap();
    // The path is supplied through the environment, never shell-interpolated.
    file.write_all(
        b"#!/bin/sh\nprintf '%s' \"$$\" > \"$DOGESH_DELIVERY_PID\"\nkill -STOP $$\nexit 0\n",
    )
    .unwrap();
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut probe = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "delivery_probe", "--nocapture"])
        .env("DOGESH_DELIVERY_PROBE", "1")
        .env("DOGESH_DELIVERY_PID", &pid_file)
        .env("DOGESH_HELPER_BIN", &script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = probe.wait_timeout(Duration::from_secs(5)).unwrap();
    if !status.is_some_and(|s| s.success()) {
        // Old pipe delivery blocks in write_all. Release its stopped reader
        // first, so both processes are cleaned up even when this test fails.
        if let Ok(pid) = std::fs::read_to_string(&pid_file)
            && let Ok(raw) = pid.parse::<i32>()
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(raw),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        if probe
            .wait_timeout(Duration::from_secs(2))
            .unwrap()
            .is_none()
        {
            let _ = probe.kill();
        }
        let _ = probe.wait();
    }
    assert!(
        status.is_some_and(|s| s.success()),
        "request delivery blocked behind a stopped helper: {status:?}"
    );
}
