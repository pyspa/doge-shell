//! Variable lookup: one name, one answer.
//!
//! The shell variable map is written to with and without a `$` prefix
//! depending on which builtin does the writing, and reads only tried one
//! spelling. `export FOO=x; echo $FOO` printed nothing while the child process
//! saw `FOO=x`.

mod common;

use common::{run_command, run_interactive};

/// Values written by `set` and `export` have to be readable by the shell
/// itself, not only by the commands it launches.
#[test]
fn set_and_export_are_readable_by_the_shell() {
    let output = run_interactive(&[
        "export FOO=exported",
        "echo $FOO",
        "set BAR plain",
        "echo $BAR",
        "echo \"quoted=$FOO\"",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    for expected in ["exported", "plain", "quoted=exported"] {
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "expected a line {expected:?} in {stdout:?}"
        );
    }
}

#[test]
fn braces_and_bare_names_resolve_the_same_way() {
    let home = std::env::var("HOME").expect("HOME");
    for command in ["echo $HOME", "echo ${HOME}", "echo \"${HOME}\""] {
        let output = run_command(command);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            home,
            "for {command:?}"
        );
    }
}

/// An indexed capture is one token. `[` is not a word character, so `$OUT[1]`
/// used to parse as `$OUT` followed by a glob and the index was lost.
#[test]
fn an_indexed_capture_keeps_its_index() {
    let output = run_interactive(&["echo captured |>", "echo \"got=$OUT[1]\""]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "got=captured"),
        "expected `got=captured` in {stdout:?}"
    );
    assert!(
        !stdout.contains("[1]"),
        "the index must not survive as literal text: {stdout:?}"
    );
}

/// A name without a sigil is a word, never a variable reference.
#[test]
fn a_bare_name_is_never_a_variable() {
    let output = run_command("echo $HOME LANG PATH");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let home = std::env::var("HOME").expect("HOME");

    assert_eq!(stdout.trim(), format!("{home} LANG PATH"));
}

/// `NAME=value cmd` sets the variable for that command only. It used to be
/// read as the command name, so the line failed with "command not found".
#[test]
fn an_assignment_prefix_reaches_the_command() {
    let output = run_command("FOO=prefixed /usr/bin/env");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line == "FOO=prefixed"),
        "expected FOO in the child environment: {stdout:?}"
    );
}

#[test]
fn several_assignments_all_reach_the_command() {
    let output = run_command("A=1 B=2 /usr/bin/env");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.lines().any(|line| line == "A=1"), "{stdout:?}");
    assert!(stdout.lines().any(|line| line == "B=2"), "{stdout:?}");
}

/// The value is expanded, and the prefix survives the alias-expansion pass --
/// which re-serializes the line and used to drop rules it did not know about.
#[test]
fn an_assignment_value_is_expanded() {
    let home = std::env::var("HOME").expect("HOME");
    let output = run_command("FOO=$HOME /usr/bin/env");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line == format!("FOO={home}")),
        "expected FOO={home} in {stdout:?}"
    );
}

/// An override replaces the inherited value rather than being appended, so the
/// child sees exactly one entry for the name.
#[test]
fn an_override_replaces_the_inherited_value() {
    let output = run_command("HOME=/overridden /usr/bin/env");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let home_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("HOME="))
        .collect();

    assert_eq!(home_lines, vec!["HOME=/overridden"], "{stdout:?}");
}

/// The prefix must not leak into the shell's own variables.
#[test]
fn a_prefix_does_not_outlive_the_command() {
    let output = run_interactive(&["FOO=temporary /bin/true", "echo \"after=$FOO\""]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "after=$FOO"),
        "the prefix should not have been kept: {stdout:?}"
    );
}

/// With no command, the assignment sets a shell variable, the way `set` does.
#[test]
fn a_standalone_assignment_sets_a_shell_variable() {
    let output = run_interactive(&["FOO=standalone", "echo $FOO"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "standalone"),
        "{stdout:?}"
    );
}

/// A builtin runs inside the shell, so a per-command environment would have to
/// be applied and unwound around the call. Refuse it rather than accepting the
/// prefix and quietly ignoring it.
#[test]
fn a_prefix_on_a_builtin_is_refused() {
    let output = run_command("FOO=bar alias");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("not supported for builtins"),
        "expected a clear refusal, got {stderr:?}"
    );
}

/// A repeated name takes the last value. The child resolves the first
/// duplicate, so the earlier one has to be dropped rather than shadowed.
#[test]
fn a_repeated_assignment_takes_the_last_value() {
    let output = run_command("A=1 A=2 /usr/bin/env");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let a_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("A="))
        .collect();

    assert_eq!(a_lines, vec!["A=2"]);
}

/// Refusing the prefix must fail that command only. Aborting the parse took the
/// rest of the line with it, including commands that had no prefix.
#[test]
fn refusing_a_builtin_prefix_does_not_abandon_the_line() {
    let output = run_command("FOO=bar alias; echo still-running");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "still-running"),
        "the rest of the line should still run: {stdout:?}"
    );
}

/// The shell has to look commands up in the `PATH` it hands its children.
/// `export PATH=...` only wrote the variable, so the child of the very next
/// command saw the new directory while the shell searching for that command did
/// not, and reported `command not found` for a tool that was right there.
#[test]
fn exporting_path_changes_where_the_shell_looks_for_commands() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let script = dir.path().join("dsh_path_probe");
    std::fs::write(&script, "#!/bin/sh\necho found-on-new-path\n").expect("failed to write probe");
    let mut permissions = std::fs::metadata(&script)
        .expect("failed to stat probe")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&script, permissions).expect("failed to chmod probe");

    let output = run_interactive(&[
        &format!("export PATH={}:$PATH", dir.path().display()),
        "dsh_path_probe",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == "found-on-new-path"),
        "the shell did not pick up the exported PATH: {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
