//! Isolated real-exec coverage: never close the test runner's own stdio.

use super::*;
use crate::environment::Environment;
use crate::process::fork::fork_process;
use crate::process::process::Process;
use crate::process::signal::send_signal_allow_gone;
use crate::process::state::ProcessState;
use crate::process::wait::{WaitPidObservation, wait_pid_job};
use crate::shell::Shell;
use nix::sys::signal::Signal;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Sole wait/signal owner from fork return until the exact child is reaped.
struct NativeChild<'a>(&'a mut Process);

impl NativeChild<'_> {
    fn observe(&mut self, no_hang: bool) -> anyhow::Result<bool> {
        let Some(pid) = self.0.pid else {
            return Ok(true);
        };
        loop {
            match wait_pid_job(pid, no_hang) {
                Ok(WaitPidObservation::State(_, state)) => {
                    self.0.state = state;
                    if matches!(state, ProcessState::Completed(..)) {
                        self.0.pid = None;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                Ok(WaitPidObservation::NoChild) => {
                    // Ownership is gone; NEVER signal this number again.
                    self.0.pid = None;
                    return Ok(true);
                }
                Ok(WaitPidObservation::StillAlive) => return Ok(false),
                Err(nix::errno::Errno::EINTR) => continue,
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn terminate_and_reap(&mut self) -> anyhow::Result<()> {
        if self.observe(true)? {
            return Ok(());
        }
        // No other waiter receives this Process. Until we reap its child,
        // even natural exit leaves a zombie reserving this exact PID.
        send_signal_allow_gone(self.0.pid.expect("unreaped child"), Signal::SIGKILL)?;
        while !self.observe(false)? {}
        Ok(())
    }

    fn wait_until(&mut self, deadline: Instant) -> anyhow::Result<bool> {
        while !self.observe(true)? {
            if Instant::now() >= deadline {
                self.terminate_and_reap()?;
                return Ok(false);
            }
            std::thread::park_timeout(Duration::from_millis(5));
        }
        Ok(true)
    }
}

impl Drop for NativeChild<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.terminate_and_reap() {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "native child cleanup failed: {error}");
        }
    }
}

const PROBE_ENV: &str = "DSH_REDIRECT_CLOSED_STDOUT_PROBE";

#[test]
fn active_duplicate_survives_exec_with_initially_closed_stdout() {
    let fixture = tempfile::tempdir().unwrap();
    let stderr_path = fixture.path().join("stderr");
    let stderr_file = File::create(&stderr_path).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process::redirect::exec_tests::closed_stdout_probe",
            "--nocapture",
        ])
        .env(PROBE_ENV, "1")
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path())
        .env("XDG_DATA_HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path())
        .env("DOGESH_ATUIN_DUAL_WRITE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            // This direct Child has not been reaped, so kill cannot target a
            // recycled PID. No group scanning or inherited-fd sweep is needed.
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::park_timeout(Duration::from_millis(5));
    };
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(
        status.is_some_and(|status| status.success()),
        "probe failed: {status:?}: {stderr}"
    );
    assert!(
        stderr.contains("ACTIVE-SNAPSHOT"),
        "active stdout disappeared at exec: {stderr}"
    );
}

#[test]
fn closed_stdout_probe() {
    if std::env::var_os(PROBE_ENV).is_none() {
        return;
    }
    // Initialize before closing stdout so unrelated setup cannot occupy it.
    let mut shell = Shell::new(Environment::new());
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.foreground = true;
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut process = Process::new(
        executable.clone(),
        vec![
            executable,
            "--exact".into(),
            "process::redirect::exec_tests::native_stdout_marker".into(),
            "--nocapture".into(),
        ],
    )
    .with_execution_metadata(Vec::new(), vec![(PROBE_ENV.into(), "native-marker".into())]);
    assert_eq!(unsafe { libc::close(STDOUT_FILENO) }, 0);
    let applied = apply(&[Redirect::dup(STDOUT_FILENO, STDERR_FILENO)], &mut ctx).unwrap();
    process.stdin = ctx.infile;
    process.stdout = ctx.outfile;
    process.stderr = ctx.errfile;
    // Exercise the production launcher and its actual child dup2-skip path.
    let (pid, monitors, failed, _report) =
        fork_process(&ctx, None, &mut process, &mut shell, None, None, None).unwrap();
    process.pid = Some(pid);
    let mut child = NativeChild(&mut process);
    applied.restore(&mut ctx);
    drop(applied);
    let completed = child
        .wait_until(Instant::now() + Duration::from_secs(2))
        .unwrap();
    // wait_until has terminated/reaped on deadline before this can panic.
    assert!(completed, "external probe did not exit");
    assert!(monitors.is_empty());
    assert!(!failed);
    assert!(
        matches!(child.0.state, ProcessState::Completed(0, _)),
        "external stdout write failed: {:?}",
        child.0.state
    );
}

/// Fixed native operation only: no interpreter or evaluated command payload.
#[test]
fn native_stdout_marker() {
    if std::env::var(PROBE_ENV).as_deref() != Ok("native-marker") {
        return;
    }
    let marker = b"ACTIVE-SNAPSHOT";
    assert_eq!(
        unsafe { libc::write(STDOUT_FILENO, marker.as_ptr().cast(), marker.len()) },
        marker.len() as isize
    );
}

/// A fixed native stop makes failure cleanup deterministic without sleep readiness.
#[test]
fn native_stopped_fixture() {
    if std::env::var(PROBE_ENV).as_deref() != Ok("native-stop") {
        return;
    }
    nix::sys::signal::raise(Signal::SIGSTOP).unwrap();
    // Cleanup uses SIGKILL even for Stopped; never silently continue the fixture.
    panic!("stopped fixture unexpectedly resumed");
}

#[test]
fn stopped_native_child_is_reaped_on_deadline_and_unwind() {
    for unwind in [false, true] {
        let fixture = tempfile::tempfile().unwrap();
        let input = File::open("/dev/null").unwrap();
        let mut shell = Shell::new(Environment::new());
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        ctx.foreground = true;
        ctx.infile = input.as_raw_fd();
        ctx.outfile = fixture.as_raw_fd();
        ctx.errfile = fixture.as_raw_fd();
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut process = Process::new(
            executable.clone(),
            vec![
                executable,
                "--exact".into(),
                "process::redirect::exec_tests::native_stopped_fixture".into(),
                "--nocapture".into(),
            ],
        )
        .with_execution_metadata(Vec::new(), vec![(PROBE_ENV.into(), "native-stop".into())]);
        process.stdin = ctx.infile;
        process.stdout = ctx.outfile;
        process.stderr = ctx.errfile;
        let (pid, monitors, failed, _report) =
            fork_process(&ctx, None, &mut process, &mut shell, None, None, None).unwrap();
        process.pid = Some(pid);
        let mut child = NativeChild(&mut process);
        assert!(!failed);
        assert!(monitors.is_empty());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(child.0.state, ProcessState::Stopped(..)) {
            assert!(!child.observe(true).unwrap(), "fixture exited before stop");
            assert!(Instant::now() < deadline, "fixture never stopped");
            std::thread::park_timeout(Duration::from_millis(5));
        }
        if unwind {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _owner = child;
                panic!("injected native probe unwind");
            }));
            // Probe the canonical waiter BEFORE asserting even the panic result.
            let observation = wait_pid_job(pid, true).unwrap();
            // Keep the regression itself leak-free if Drop is broken: the
            // original Process remains ours, and canonical observation avoids
            // signaling any PID whose terminal status was already consumed.
            NativeChild(&mut process).terminate_and_reap().unwrap();
            assert!(
                matches!(observation, WaitPidObservation::NoChild),
                "unwind left {observation:?}"
            );
            assert!(result.is_err());
        } else {
            let completed = child.wait_until(Instant::now()).unwrap();
            let observation = wait_pid_job(pid, true).unwrap();
            child.terminate_and_reap().unwrap();
            assert!(
                matches!(observation, WaitPidObservation::NoChild),
                "deadline left {observation:?}"
            );
            assert!(!completed);
            assert!(matches!(
                child.0.state,
                ProcessState::Completed(_, Some(Signal::SIGKILL))
            ));
        }
    }
}
