//! Isolated real-exec coverage: never close the test runner's own stdio.

use super::*;
use crate::environment::Environment;
use crate::process::fork::fork_process;
use crate::process::process::Process;
use crate::process::state::ProcessState;
use crate::shell::Shell;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    let (pid, monitors, failed) =
        fork_process(&ctx, None, &mut process, &mut shell, None, None, None).unwrap();
    applied.restore(&mut ctx);
    drop(applied);
    process.pid = Some(pid);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !matches!(process.update_state(), Some(ProcessState::Completed(..))) {
        assert!(Instant::now() < deadline, "external probe did not exit");
        std::thread::park_timeout(Duration::from_millis(5));
    }
    assert!(monitors.is_empty());
    assert!(!failed);
    assert!(
        matches!(process.state, ProcessState::Completed(0, _)),
        "external stdout write failed: {:?}",
        process.state
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
