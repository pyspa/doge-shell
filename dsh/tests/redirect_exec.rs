//! Real-exec descriptor lifetimes using fixed native Rust fixture operations.
//! Helpers are inert unless the owning test supplies an isolated fixture path.

mod common;

use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const FIXTURE_ENV: &str = "DSH_REDIRECT_NATIVE_FIXTURE";

#[test]
fn native_producer() {
    let Some(fixture) = std::env::var_os(FIXTURE_ENV).map(PathBuf::from) else {
        return;
    };
    fs::write(fixture.join("ready"), b"ready").unwrap();
    let gate = fs::File::open(fixture.join("release")).unwrap();
    let mut release = String::new();
    std::io::BufReader::new(gate)
        .read_line(&mut release)
        .unwrap();
    assert_eq!(release, "release\n");
}

#[test]
fn native_consumer() {
    let Some(fixture) = std::env::var_os(FIXTURE_ENV).map(PathBuf::from) else {
        return;
    };
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    fs::write(fixture.join("eof"), b"eof").unwrap();
}

/// An overwritten snapshot must release the pipeline writer on exec while the
/// producer remains alive behind an explicit FIFO release handshake.
#[test]
fn overwritten_duplicate_does_not_delay_pipeline_eof() {
    let _serial = common::serial_guard();
    let fixture = tempfile::tempdir().unwrap();
    let gate = fixture.path().join("release");
    nix::unistd::mkfifo(
        &gate,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    // RDWR does not wait for the reader. Rust opens it CLOEXEC, and a tiny
    // release message fits even if launch fails before the producer starts.
    let mut release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&gate)
        .unwrap();
    let executable = std::env::current_exe().unwrap();
    let fixture_arg = shell_words::quote(&fixture.path().to_string_lossy()).into_owned();
    let executable_arg = shell_words::quote(&executable.to_string_lossy()).into_owned();
    let command = format!(
        "{FIXTURE_ENV}={fixture_arg} {executable_arg} --exact native_producer --nocapture 2>&1 > out 2> err | {FIXTURE_ENV}={fixture_arg} {executable_arg} --exact native_consumer --nocapture"
    );
    // Product entry and BOTH commands still pass the normal SafetyGuard.
    let child = common::process::spawn_dsh_unlocked(["-c", &command], None);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !(fixture.path().join("ready").exists() && fixture.path().join("eof").exists())
        && Instant::now() < deadline
    {
        std::thread::park_timeout(Duration::from_millis(5));
    }
    let ready_before_release = fixture.path().join("ready").exists();
    let eof_before_release = fixture.path().join("eof").exists();
    // Always release before assertions, including timeout: canonical shell Job
    // ownership then reaps both stages. No new PID/group sweep or signal code.
    let released = writeln!(release, "release");
    let output = child
        .wait(Duration::from_secs(5))
        .expect("released pipeline finishes");
    released.unwrap();
    assert!(
        output.status.success(),
        "native pipeline failed: {output:?}, ready={ready_before_release}, eof={eof_before_release}"
    );
    assert!(
        ready_before_release,
        "producer never reached its read handshake"
    );
    assert!(
        eof_before_release,
        "overwritten duplicate held the pipe writer until producer exit"
    );
}
