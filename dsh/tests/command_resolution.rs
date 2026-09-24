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
    let output = run_interactive(&[
        common::true_path(),
        "definitely-not-a-command-xyz",
        "echo rc=$?",
    ]);
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

fn chmod_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("failed to stat script")
        .permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions).expect("failed to chmod script");
}

/// A relative `PATH` (`bin1:bin2`) resolves against the current directory on
/// every lookup: caching a resolution across `cd` would answer with the old
/// directory's executable.
#[test]
fn a_relative_path_follows_the_current_directory() {
    let root = tempfile::tempdir().expect("failed to create temp dir");
    for sub in ["A/bin1", "A/bin2", "B/bin1", "B/bin2"] {
        std::fs::create_dir_all(root.path().join(sub)).expect("failed to create dir");
    }
    write_script(&root.path().join("A/bin2"), "foo", "A2");
    write_script(&root.path().join("B/bin1"), "foo", "B1");
    write_script(&root.path().join("B/bin2"), "foo", "B2");

    let output = run_interactive(&[
        &format!("cd {}", root.path().join("A").display()),
        "PATH=bin1:bin2",
        "foo",
        &format!("cd {}", root.path().join("B").display()),
        "foo",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout.lines().map(|line| line.trim().to_string()).collect();
    let first = lines.iter().position(|line| line == "A2");
    let second = lines.iter().position(|line| line == "B1");
    assert!(
        first.is_some() && second.is_some() && first.unwrap() < second.unwrap(),
        "expected A2 then B1 in {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `B2` must not answer: `bin1` wins in the new directory.
    let b2_after_move = lines
        .iter()
        .skip(second.unwrap())
        .take(3)
        .any(|line| line == "B2");
    assert!(
        !b2_after_move,
        "relative PATH result was reused across cd: {stdout:?}"
    );
}

/// A non-executable regular file in `PATH` is skipped, not executed.
#[test]
fn a_non_executable_path_entry_is_skipped() {
    let dir_a = tempfile::tempdir().expect("failed to create temp dir");
    let dir_b = tempfile::tempdir().expect("failed to create temp dir");
    let script_a = write_script(dir_a.path(), "foo", "marker-A");
    chmod_mode(&script_a, 0o644);
    write_script(dir_b.path(), "foo", "marker-B");

    let output = run_command(&format!(
        "export PATH={}:{}:$PATH; foo",
        dir_a.path().display(),
        dir_b.path().display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "marker-B"),
        "expected the executable candidate in {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Without a negative cache, a command that missed once is found after it is
/// installed (or made executable) later in the same session.
#[test]
fn a_previous_miss_does_not_hide_a_newly_executable_command() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let script = write_script(dir.path(), "foo", "now-runs");
    chmod_mode(&script, 0o644);

    let output = run_interactive(&[
        &format!("export PATH={}:$PATH", dir.path().display()),
        "foo",
        &format!("chmod +x {}", script.display()),
        "foo",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("command not found"),
        "expected the first lookup to miss in {stderr:?}"
    );
    assert!(
        stdout.lines().any(|line| line.trim() == "now-runs"),
        "expected the second lookup to run in {stdout:?}"
    );
}

/// Assigning `PATH` invalidates remembered locations even when the textual
/// value is unchanged, so a newly preferred candidate is picked up.
#[test]
fn a_same_value_path_assignment_invalidates_the_cached_location() {
    let dir_a = tempfile::tempdir().expect("failed to create temp dir");
    let dir_b = tempfile::tempdir().expect("failed to create temp dir");
    let script_a = write_script(dir_a.path(), "foo", "marker-A");
    chmod_mode(&script_a, 0o644);
    write_script(dir_b.path(), "foo", "marker-B");

    let output = run_interactive(&[
        &format!(
            "export PATH={}:{}:$PATH",
            dir_a.path().display(),
            dir_b.path().display()
        ),
        "foo",
        &format!("chmod +x {}", script_a.display()),
        "PATH=$PATH",
        "foo",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout.lines().map(|line| line.trim().to_string()).collect();
    let first = lines.iter().position(|line| line == "marker-B");
    let second = lines.iter().rposition(|line| line == "marker-A");
    assert!(
        first.is_some() && second.is_some() && first.unwrap() < second.unwrap(),
        "expected marker-B then marker-A in {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `sub/probe` is an explicit pathname: it must not be searched as
/// `$PATH/sub/probe`.
#[test]
fn a_slash_containing_relative_command_bypasses_path_search() {
    let work = tempfile::tempdir().expect("failed to create temp dir");
    std::fs::create_dir(work.path().join("sub")).expect("failed to create sub");
    write_script(&work.path().join("sub"), "probe", "probe-runs");

    let output = run_command(&format!(
        "cd {} && PATH=/definitely/not/a/real/path; sub/probe",
        work.path().display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "probe-runs"),
        "sub/probe did not run: {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `PATH=/custom foo` looks the command up in `/custom` and hands the child
/// the same `PATH`, without leaking the override into the shell or its cache.
#[test]
fn a_command_scoped_path_selects_that_commands_lookup() {
    let dir_a = tempfile::tempdir().expect("failed to create temp dir");
    let dir_b = tempfile::tempdir().expect("failed to create temp dir");
    write_script(dir_a.path(), "foo", "marker-A");
    write_script(dir_b.path(), "foo", "marker-B");

    let output = run_interactive(&[
        &format!("export PATH={}:$PATH", dir_a.path().display()),
        &format!("PATH={} foo", dir_b.path().display()),
        "foo",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout.lines().map(|line| line.trim().to_string()).collect();
    let first = lines.iter().position(|line| line == "marker-B");
    let second = lines.iter().rposition(|line| line == "marker-A");
    assert!(
        first.is_some() && second.is_some() && first.unwrap() < second.unwrap(),
        "expected marker-B then marker-A in {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A duplicated scoped `PATH` keeps last-assignment-wins, matching the child
/// environment the command actually runs with.
#[test]
fn a_duplicate_command_scoped_path_keeps_the_last_assignment() {
    let dir_a = tempfile::tempdir().expect("failed to create temp dir");
    let dir_b = tempfile::tempdir().expect("failed to create temp dir");
    write_script(dir_a.path(), "foo", "marker-A");
    write_script(dir_b.path(), "foo", "marker-B");

    let output = run_command(&format!(
        "export PATH={}:$PATH; PATH={} PATH={} foo",
        dir_a.path().display(),
        dir_a.path().display(),
        dir_b.path().display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "marker-B"),
        "expected the last scoped PATH to win in {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
