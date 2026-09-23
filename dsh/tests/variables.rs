//! Variable lookup: one logical variable, one stored value.
//!
//! `variable_state.variables` is the single value storage and
//! `exported_vars` carries only the export attribute. `$FOO`, `${FOO}` and
//! `FOO` canonicalize to one `FOO` key, never two entries.

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
    let output = run_interactive(&[
        &format!("FOO=temporary {}", common::true_path()),
        "echo \"after=$FOO\"",
    ]);
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
    // The trailing `echo` succeeded, so the line as a whole exits zero even
    // though the refused command itself failed (see the `$?` tests below).
    assert!(
        output.status.success(),
        "the succeeding tail must carry the line: {:?}",
        output.status
    );
}

/// The refusal is a real command failure: stderr names it and the `dogesh`
/// process exits non-zero, instead of looking like success.
#[test]
fn builtin_assignment_prefix_refusal_is_nonzero() {
    let output = run_command("FOO=bar alias");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("not supported for builtins"),
        "expected a clear refusal, got {stderr:?}"
    );
    assert!(
        !output.status.success(),
        "a refused builtin prefix must fail: {:?}",
        output.status
    );
}

/// `&&` is gated off by the refusal: the branch must not run.
#[test]
fn builtin_assignment_prefix_refusal_blocks_and_branch() {
    let output = run_command("FOO=bar alias && echo SHOULD_NOT_RUN");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("SHOULD_NOT_RUN"),
        "the && branch must not run after a refusal: {stdout:?}"
    );
    assert!(
        !output.status.success(),
        "the gated line must stay failed: {:?}",
        output.status
    );
}

/// `||` is gated on by the refusal: the branch must run.
#[test]
fn builtin_assignment_prefix_refusal_runs_or_branch() {
    let output = run_command("FOO=bar alias || echo EXPECTED_FAILURE");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "EXPECTED_FAILURE"),
        "the || branch must run after a refusal: {stdout:?}"
    );
}

/// The refusal publishes its own status: a previous success must not linger
/// in `$?`.
#[test]
fn builtin_assignment_prefix_refusal_updates_last_status() {
    let output = run_command("true; FOO=bar alias; echo status=$?");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "status=1"),
        "expected the refusal status, got {stdout:?}"
    );
}

/// A previous failure must not linger either: the refusal overwrites `$?`
/// with its own status instead of keeping the stale one (127 here).
#[test]
fn builtin_assignment_prefix_refusal_overwrites_previous_failure() {
    let output = run_command("dsh-nonexistent-probe-xyz; FOO=bar alias; echo status=$?");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "status=1"),
        "expected the refusal status, not the stale 127: {stdout:?}"
    );
}

fn write_marker_script(
    dir: &tempfile::TempDir,
    name: &str,
    marker_name: &str,
) -> std::path::PathBuf {
    let marker = dir.path().join(marker_name);
    let path = dir.path().join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nprintf BAD > \"{}\"", marker.display()),
    )
    .expect("write marker script");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat marker script")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod marker script");
    path
}

/// A rejected stage must not be dropped from its pipeline: the whole job is
/// refused before launch, so the downstream stage never runs.
#[test]
fn rejected_builtin_prefix_aborts_pipeline_before_launch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("downstream_marker");
    let downstream = write_marker_script(&dir, "downstream.sh", "downstream_marker");
    let line = format!("FOO=bar alias | {}", downstream.display());
    let output = run_interactive(&[line.as_str()]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("not supported for builtins"),
        "expected the refusal diagnostic, got {stderr:?}"
    );
    assert!(
        !marker.exists(),
        "the downstream stage must not launch after a rejection"
    );
}

/// Same invariant for a rejected middle stage: neither the upstream nor the
/// downstream stage may launch, and the pipeline must not collapse to the
/// surviving stages.
#[test]
fn rejected_middle_builtin_stage_does_not_collapse_pipeline() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let up_marker = dir.path().join("up_marker");
    let down_marker = dir.path().join("down_marker");
    let upstream = write_marker_script(&dir, "upstream.sh", "up_marker");
    let downstream = write_marker_script(&dir, "downstream2.sh", "down_marker");
    let line = format!(
        "{} | FOO=bar alias | {}",
        upstream.display(),
        downstream.display()
    );
    let output = run_interactive(&[line.as_str()]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("not supported for builtins"),
        "expected the refusal diagnostic, got {stderr:?}"
    );
    assert!(
        !up_marker.exists(),
        "the upstream stage must not launch after a rejection"
    );
    assert!(
        !down_marker.exists(),
        "the downstream stage must not launch after a rejection"
    );
}

/// The isolated helper evaluator shares the top-level semantics: `||`
/// observes the refusal inside `$(...)`.
#[test]
fn builtin_prefix_refusal_in_command_substitution_runs_or_branch() {
    let output = run_command("echo \"$(FOO=bar alias || echo helper-refusal-observed)\"");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == "helper-refusal-observed"),
        "the helper || branch must run: {stdout:?}"
    );
}

/// The isolated helper evaluator gates `&&` off the refusal like the top
/// level does.
#[test]
fn builtin_prefix_refusal_in_command_substitution_blocks_and_branch() {
    let output = run_command("echo \"$(FOO=bar alias && echo SHOULD_NOT_RUN)\"");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("SHOULD_NOT_RUN"),
        "the helper && branch must not run: {stdout:?}"
    );
}

/// `A=$(false)` sets `A` in the current shell while the simple command
/// itself reports the substitution status 1.
#[test]
fn assignment_with_failing_substitution_sets_var_and_reports_status() {
    let output = run_command(&format!(
        "A=$({}); /bin/echo \"status=$?\"; /bin/echo \"A=[$A]\"",
        common::false_path()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "status=1"),
        "expected status=1 from A=$(false), got {stdout:?}"
    );
    // `A` is set (to the empty substitution output): an unset name would
    // echo back literally as `[$A]`, while the set-but-empty value is `[]`.
    assert!(
        stdout.lines().any(|line| line.trim() == "A=[]"),
        "expected A to exist in the shell, got {stdout:?}"
    );
}

/// Assignments apply before redirections: even when the redirect fails, the
/// variable is already in the shell while the command reports the failure.
#[test]
fn assignment_survives_failed_redirect() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-dir").join("out");
    let output = run_command(&format!(
        "FOO=bar > {}; /bin/echo \"FOO=[$FOO] status=$?\"",
        missing.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == "FOO=[bar] status=1"),
        "expected the assignment plus the redirect failure, got {stdout:?}"
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

fn write_read_input(dir: &tempfile::TempDir, name: &str, contents: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write read input");
    path
}

/// `read` overwrites the canonical shell variable, not a `$`-prefixed copy.
#[test]
fn read_overwrites_an_existing_variable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_new.txt", b"new-value\n");
    let output = run_command(&format!(
        "set FOO old; read FOO < {}; echo $FOO",
        input.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "new-value"),
        "read did not overwrite FOO: {stdout:?} stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `read` then `export` reaches the child environment.
#[test]
fn read_then_export_reaches_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_export.txt", b"new-value\n");
    let output = run_command(&format!(
        "read FOO < {}; export FOO; /usr/bin/env",
        input.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line == "FOO=new-value"),
        "child missed FOO=new-value: {stdout:?}"
    );
}

/// `read` consumes exactly one line.
#[test]
fn read_consumes_only_the_first_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_two.txt", b"first\nsecond\n");
    let output = run_command(&format!("read FOO < {}; echo $FOO", input.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "first"),
        "expected only the first line: {stdout:?}"
    );
    assert!(
        !stdout.contains("second"),
        "read consumed past the first line: {stdout:?}"
    );
}

/// An empty line is success with an empty value.
#[test]
fn read_empty_line_is_success_with_empty_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_empty.txt", b"\n");
    let output = run_command(&format!(
        "read FOO < {}; echo \"value=[$FOO] status=$?\"",
        input.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == "value=[] status=0"),
        "empty line mishandled: {stdout:?}"
    );
}

/// EOF with no bytes is status 1 and stays silent.
#[test]
fn read_empty_file_reports_eof_silently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_eof.txt", b"");
    let output = run_command(&format!("read FOO < {}; echo $?", input.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.lines().any(|line| line.trim() == "1"),
        "EOF must report status 1: {stdout:?}"
    );
    assert!(
        !stderr.contains("read:"),
        "EOF must not emit a diagnostic: {stderr:?}"
    );
}

/// A final line without a newline still sets the value but reports EOF.
#[test]
fn read_unterminated_last_line_sets_value_with_eof_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_unterminated.txt", b"abc");
    let output = run_command(&format!(
        "read FOO < {}; echo \"value=[$FOO] status=$?\"",
        input.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == "value=[abc] status=1"),
        "unterminated line mishandled: {stdout:?}"
    );
}

/// Unsupported shapes never run partially.
#[test]
fn read_rejects_an_invalid_name() {
    let output = run_command("read 1BAD");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid name must fail: {:?}",
        output.status
    );
    assert!(
        stderr.contains("read:"),
        "invalid name needs a diagnostic: {stderr:?}"
    );
}

/// Extra operands and options are usage errors, not partial runs.
#[test]
fn read_rejects_unsupported_shapes() {
    for command in ["read", "read FOO BAR", "read -r FOO"] {
        let output = run_command(command);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{command:?} must fail: {:?}",
            output.status
        );
        assert!(
            stderr.contains("read:"),
            "{command:?} needs a diagnostic: {stderr:?}"
        );
    }
}

/// Invalid UTF-8 never partially updates the variable.
#[test]
fn read_invalid_utf8_keeps_the_old_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_invalid.txt", &[0xff, 0xfe, b'\n']);
    let output = run_command(&format!(
        "set FOO old; read FOO < {}; echo \"value=[$FOO] status=$?\"",
        input.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == "value=[old] status=1"),
        "invalid UTF-8 must not mutate FOO: {stdout:?}"
    );
    assert!(
        stderr.contains("read:"),
        "invalid UTF-8 needs a diagnostic: {stderr:?}"
    );
}

/// `var` never shows a duplicate sigil spelling for one variable.
#[test]
fn var_lists_each_variable_once_without_sigil() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_read_input(&dir, "read_var.txt", b"new-value\n");
    let output = run_command(&format!("set FOO old; read FOO < {}; var", input.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("$FOO"),
        "var must not list a sigil spelling: {stdout:?}"
    );
    assert!(
        stdout.contains("FOO") && stdout.contains("new-value"),
        "var must list the canonical value: {stdout:?}"
    );
}
