//! A command name is resolved when the command runs, not when the line is read.
//!
//! Resolving during the parse answered with the directory and the `PATH` from
//! before anything on the line had run, and a name that did not resolve threw
//! the whole line away instead of failing that one command.

mod common;

use common::{run_command, run_dsh, run_interactive};
use std::time::Duration;

/// A script in `dir`, executable, printing `marker`.
fn write_script(dir: &std::path::Path, name: &str, marker: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\necho {marker}\n")).expect("failed to write script");
    let mut permissions = std::fs::metadata(&path)
        .expect("failed to stat script")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&path, permissions).expect("failed to chmod script");
    path
}

/// The most ordinary thing anyone does with `&&`. `./script` used to be looked
/// up against the directory the shell was in *before* the `cd` ran.
#[test]
fn a_relative_command_resolves_after_the_cd_that_precedes_it() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    write_script(dir.path(), "probe.sh", "ran-after-cd");

    for separator in ["&&", ";"] {
        let command = format!("cd {} {separator} ./probe.sh", dir.path().display());
        let output = run_command(&command);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            stdout.lines().any(|line| line.trim() == "ran-after-cd"),
            "{command:?} did not run the script: {stdout:?}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A typo is one failed command, not a reason to discard the commands around it.
#[test]
fn an_unknown_command_does_not_abandon_the_line() {
    let output = run_command("/bin/echo before; definitely-not-a-command-xyz; /bin/echo after");
    let stdout = String::from_utf8_lossy(&output.stdout);

    for expected in ["before", "after"] {
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "expected a line {expected:?} in {stdout:?}"
        );
    }
}

/// Which means `||` can react to it.
#[test]
fn an_unknown_command_lets_the_fallback_run() {
    let output = run_command("definitely-not-a-command-xyz || /bin/echo fallback");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "fallback"),
        "the fallback never ran: {stdout:?}"
    );

    let skipped = run_command("definitely-not-a-command-xyz && /bin/echo yes");
    let skipped = String::from_utf8_lossy(&skipped.stdout);
    assert!(
        !skipped.contains("yes"),
        "`&&` ran after a failed command: {skipped:?}"
    );
}

/// And `$?` reports it, the way it reports any other failure.
#[test]
fn an_unknown_command_sets_the_exit_status_to_127() {
    let output = run_interactive(&["/bin/true", "definitely-not-a-command-xyz", "echo rc=$?"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "rc=127"),
        "expected rc=127 in {stdout:?}"
    );
}

/// The message belongs to the failing command, so redirecting its stderr
/// silences it.
#[test]
fn the_not_found_message_goes_to_the_commands_stderr() {
    let noisy = run_command("definitely-not-a-command-xyz");
    assert!(
        String::from_utf8_lossy(&noisy.stderr).contains("command not found"),
        "expected a diagnostic on stderr"
    );

    let quiet = run_dsh(
        ["-c", "definitely-not-a-command-xyz 2>/dev/null"],
        Duration::from_secs(10),
    );
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("command not found"),
        "redirecting stderr should silence it: {:?}",
        String::from_utf8_lossy(&quiet.stderr)
    );
}

/// A `PATH` exported earlier on the same line is the one the command is looked
/// up in.
#[test]
fn a_path_exported_earlier_on_the_line_is_used() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    write_script(dir.path(), "dsh_same_line_probe", "found-later");

    let output = run_command(&format!(
        "export PATH={}:$PATH; dsh_same_line_probe",
        dir.path().display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "found-later"),
        "the exported PATH was not used: {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
