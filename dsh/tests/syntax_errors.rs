//! Execution planning is strict: any non-whitespace unparsed tail is a syntax
//! error and no prefix is executed. `Rule::commands` remains tolerant for REPL
//! highlighting and completion.

mod common;

use common::{run_command, run_interactive};

/// Inputs that stay malformed no matter how far the parser work goes, so these
/// assertions are stable across the rest of the foundation stages.
#[test]
fn malformed_input_is_a_syntax_error_with_no_execution() {
    for (command, tail) in [
        ("echo a )", ")"),
        ("echo a &&&", "&"),
        ("echo a (((", "((("),
        ("echo unterminated\"", "\""),
    ] {
        let output = run_command(command);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            stderr.contains("syntax error"),
            "expected a syntax error for {command:?}, got stderr:\n{stderr}"
        );
        assert!(
            stderr.contains(tail),
            "expected the error for {command:?} to name the leftover {tail:?}, got stderr:\n{stderr}"
        );
        assert!(
            !output.status.success(),
            "expected non-zero exit for {command:?}"
        );
        assert!(
            !stdout.contains('a'),
            "malformed input must not execute its prefix for {command:?}, got stdout:\n{stdout}"
        );
    }
}

#[test]
fn ordinary_commands_do_not_error() {
    for command in [
        "echo hello",
        "echo a; echo b",
        "echo a && echo b",
        "echo a || echo b",
        "echo a | cat",
        "echo 'single quoted'",
        "echo \"double quoted\"",
        "echo x > /dev/null",
        "echo trailing;",
        "echo spaced   ",
    ] {
        let output = run_command(command);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            !stderr.contains("syntax error"),
            "unexpected syntax error for {command:?}:\n{stderr}"
        );
        assert!(
            output.status.success(),
            "expected success for {command:?}, got stderr:\n{stderr}"
        );
    }
}

/// The parsed prefix must never run when the line is malformed.
#[test]
fn malformed_input_never_executes_the_parsed_prefix() {
    let output = run_command("echo kept )");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stdout.contains("kept"),
        "the parsed prefix must not execute, got stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("syntax error"),
        "expected a syntax error, got stderr:\n{stderr}"
    );
    assert!(
        !output.status.success(),
        "expected non-zero exit for malformed input"
    );
}

/// A redirect in a malformed line must not create or truncate its target.
#[test]
fn malformed_input_creates_no_redirect_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");
    let command = format!("printf kept > {} )", marker.display());
    let output = run_command(&command);
    assert!(
        !output.status.success(),
        "expected non-zero exit for {command:?}"
    );
    assert!(
        !marker.exists(),
        "malformed line must not create its redirect target"
    );
}

/// A substitution in a malformed line must never run.
#[test]
fn malformed_input_runs_no_substitution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("subst_marker");
    let command = format!("echo $(touch {}) )", marker.display());
    let output = run_command(&command);
    assert!(
        !output.status.success(),
        "expected non-zero exit for {command:?}"
    );
    assert!(
        !marker.exists(),
        "malformed line must not run its substitution"
    );
}

/// Expansion must not discard a raw suffix: `echo $HOME > marker )` goes
/// through the meta-expansion path, but the raw `)` still rejects the line.
#[test]
fn expansion_path_keeps_the_raw_tail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("expansion_marker");
    let command = format!("echo $HOME > {} )", marker.display());
    let output = run_command(&command);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("syntax error"),
        "expected a syntax error for {command:?}, got stderr:\n{stderr}"
    );
    assert!(
        !output.status.success(),
        "expected non-zero exit for {command:?}"
    );
    assert!(
        !marker.exists(),
        "expansion must not hide the raw tail and run the prefix"
    );
}

/// A syntax error must not end the session: the next line still runs.
#[test]
fn interactive_session_continues_after_syntax_error() {
    let output = run_interactive(&["echo SHOULD_NOT_RUN )", "echo AFTER_ERROR"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stdout.contains("SHOULD_NOT_RUN"),
        "malformed prefix must not run, got stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("AFTER_ERROR"),
        "session must continue after a syntax error, got stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("syntax error"),
        "expected a syntax error on stderr, got:\n{stderr}"
    );
}

/// A syntax error still records a non-zero `$?` instead of leaving the
/// previous line's success in place.
#[test]
fn syntax_error_publishes_nonzero_exit_status() {
    let output = run_interactive(&["echo bad )", "echo $?"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.lines().any(|line| line.trim() == "1"),
        "expected `$?` to be 1 after a syntax error, got stdout:\n{stdout}"
    );
}
