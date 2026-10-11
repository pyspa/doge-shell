//! Captured Lisp shell execution on an owned controlling PTY.
mod common;

use nix::pty::openpty;
use nix::unistd::Pid;
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn capture(source: &str, limit: bool) -> Output {
    let root = tempfile::tempdir().unwrap();
    let payload = "日本語 abc 0123456789\n".repeat(160_000);
    fs::write(root.path().join("payload"), &payload).unwrap();
    fs::write(root.path().join("program.lisp"), source).unwrap();
    fs::write(
        root.path().join("interrupted.sh"),
        "cat payload\nkill -INT $$\n",
    )
    .unwrap();
    let pty = openpty(None, None).unwrap();
    let _master = File::from(pty.master);
    let mut command = Command::new(env!("CARGO_BIN_EXE_dogesh"));
    command
        .current_dir(root.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(common::process::contract_env(&root))
        .env("SAFETY_LEVEL", "loose")
        .args(["lisp", "program.lisp"])
        .stdin(Stdio::from(File::from(pty.slave)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if limit {
                let bound = libc::rlimit {
                    rlim_cur: 96,
                    rlim_max: 96,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &bound) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let group = Pid::from_raw(child.id() as i32);
    let mut result =
        common::process::wait_child_with_output_deadline(child, group, Duration::from_secs(30))
            .expect("capture must complete within the deadline");
    // Interactive execution writes OSC titles to the outer stdout. They are
    // presentation, not bytes returned by sh!; the fixture contains no ESC.
    let mut plain = Vec::new();
    let mut i = 0;
    while i < result.stdout.len() {
        if result.stdout[i..].starts_with(b"\x1b]") {
            i += result.stdout[i..]
                .iter()
                .position(|&b| b == 7)
                .expect("title terminator")
                + 1;
        } else {
            plain.push(result.stdout[i]);
            i += 1;
        }
    }
    result.stdout = plain;
    result
}

#[test]
fn large_capture_is_exact_with_stderr_pipeline_and_nonzero_exit() {
    for source in [
        "(sh! \"cat payload\")",
        "(sh! \"cat payload | cat\")",
        "(sh! \"cat payload; echo diagnostic >&2; false\")",
    ] {
        let result = capture(source, false);
        assert!(result.status.success(), "{:?}", result.stderr);
        let expected = "日本語 abc 0123456789\n".repeat(160_000);
        let actual = if source.contains("diagnostic") {
            // Existing stderr monitors render diagnostics on outer stdout.
            let prefix = b"diagnostic\r\n";
            assert!(
                result.stdout.starts_with(prefix),
                "stderr diagnostic must be presented separately"
            );
            &result.stdout[prefix.len()..]
        } else {
            result.stdout.as_slice()
        };
        assert!(
            actual == expected.as_bytes(),
            "capture length {}, expected {}",
            result.stdout.len(),
            expected.len()
        );
    }
}

#[test]
fn repeated_capture_releases_fds() {
    let source = "(sh! \"echo small\")\n".repeat(128) + "\"done\"";
    let result = capture(&source, true);
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(result.stdout, b"done\n");
}

#[test]
fn interrupted_capture_returns_and_can_run_again() {
    let result = capture(
        "(print (sh! \"sh interrupted.sh\"))\n(sh! \"echo after\")",
        false,
    );
    assert!(result.status.success(), "{:?}", result.stderr);
    let expected = "日本語 abc 0123456789\n".repeat(160_000) + "after\n";
    assert!(
        result.stdout == expected.as_bytes(),
        "interrupted capture must preserve every byte and allow another call"
    );
}
