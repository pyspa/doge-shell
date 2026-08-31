#![allow(dead_code)]

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tempfile::TempDir;
use wait_timeout::ChildExt;

fn child_process_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn run_dsh<I, S>(args: I, timeout: Duration) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_dsh_with_input(args, None, timeout)
}

pub fn run_dsh_with_input<I, S>(args: I, input: Option<&str>, timeout: Duration) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let _guard = child_process_lock();
    let temp = TempDir::new().expect("failed to create isolated dsh test directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_dsh"))
        .args(args)
        .env("XDG_STATE_HOME", temp.path())
        .env("XDG_DATA_HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("failed to spawn dsh");

    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("failed to open dsh stdin");
        stdin
            .write_all(input.as_bytes())
            .expect("failed to write dsh stdin");
    }

    if child
        .wait_timeout(timeout)
        .expect("failed while waiting for dsh")
        .is_none()
    {
        let process_group = nix::unistd::Pid::from_raw(child.id() as i32);
        match nix::sys::signal::killpg(process_group, nix::sys::signal::Signal::SIGKILL) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(err) => {
                let _ = child.kill();
                panic!("failed to kill timed-out dsh process group: {err}");
            }
        }
        let output = child
            .wait_with_output()
            .expect("failed to collect timed-out dsh output");
        panic!(
            "dsh did not exit within {timeout:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    child
        .wait_with_output()
        .expect("failed to collect dsh output")
}

/// Absolute path to an external `true`.
///
/// The tests spell these out absolutely so the shell resolves a real external
/// command instead of a builtin, but the location differs by platform: macOS
/// ships them only under `/usr/bin`, while some Linux layouts have them only
/// under `/bin`.
pub fn true_path() -> &'static str {
    first_existing(&["/bin/true", "/usr/bin/true"])
}

/// Absolute path to an external `false`. See [`true_path`].
pub fn false_path() -> &'static str {
    first_existing(&["/bin/false", "/usr/bin/false"])
}

fn first_existing(candidates: &'static [&'static str]) -> &'static str {
    candidates
        .iter()
        .copied()
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| panic!("none of {candidates:?} exist on this system"))
}

pub fn run_command(command: &str) -> Output {
    run_dsh(["-c", command], Duration::from_secs(10))
}

pub fn run_interactive(lines: &[&str]) -> Output {
    let mut input = lines.join("\n");
    input.push_str("\nexit\n");
    run_dsh_with_input(
        std::iter::empty::<&str>(),
        Some(&input),
        Duration::from_secs(10),
    )
}
