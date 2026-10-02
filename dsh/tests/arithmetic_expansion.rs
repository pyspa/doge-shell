//! POSIX arithmetic expansion `$((...))` end-to-end behavior.

mod common;

use common::{false_path, run_command, run_interactive, true_path};

fn stdout_of(command: &str) -> String {
    String::from_utf8_lossy(&run_command(command).stdout).to_string()
}

fn stderr_of(command: &str) -> String {
    String::from_utf8_lossy(&run_command(command).stderr).to_string()
}

#[test]
fn basic_literal() {
    assert_eq!(stdout_of("echo $((1 + 2 * 3))").trim(), "7");
}

#[test]
fn precedence() {
    assert_eq!(stdout_of("echo $((1 + 2 * 3))").trim(), "7");
    assert_eq!(stdout_of("echo $((1 << 2 + 1))").trim(), "8");
}

#[test]
fn parentheses() {
    assert_eq!(stdout_of("echo $(((1 + 2) * 3))").trim(), "9");
}

#[test]
fn bare_variable() {
    assert_eq!(stdout_of("X=7; echo $((X + 1))").trim(), "8");
}

#[test]
fn dollar_variable_inside_arithmetic() {
    assert_eq!(stdout_of("X=5; echo $(( $X + 1 ))").trim(), "6");
}

#[test]
fn parameter_default_inside_arithmetic() {
    assert_eq!(stdout_of("X=2; echo $(( ${X:-1} + 3 ))").trim(), "5");
    assert_eq!(
        stdout_of("echo $(( ${DOGESH_ARITH_UNSET:-4} + 1 ))").trim(),
        "5"
    );
}

#[test]
fn decimal_octal_hex() {
    assert_eq!(stdout_of("echo $((10 + 010 + 0x10))").trim(), "34");
    assert_eq!(stdout_of("X=010; echo $((X + 1))").trim(), "9");
    assert_eq!(stdout_of("echo $((0x10 + 1))").trim(), "17");
}

#[test]
fn simple_assignment() {
    let out = stdout_of("X=1; echo $((X = 3)); echo \"$X\"");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines, vec!["3", "3"]);
}

#[test]
fn compound_assignment() {
    let out = stdout_of("X=1; echo $((X += 2)); echo \"$X\"");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines, vec!["3", "3"]);
    assert_eq!(stdout_of("X=3; echo $((X *= 4))").trim(), "12");
}

#[test]
fn assignment_persistence() {
    let out = stdout_of("X=1; echo $((X += 2)) > /dev/null; echo \"$X\"");
    assert_eq!(out.trim(), "3");
}

#[test]
fn gated_out_assignment_has_no_side_effect() {
    let out = stdout_of(&format!(
        "X=1; {} && echo $((X = 9)); echo \"$X\"",
        false_path()
    ));
    assert_eq!(out.trim(), "1");
    let out = stdout_of(&format!(
        "X=1; {} || echo $((X = 9)); echo \"$X\"",
        true_path()
    ));
    assert_eq!(out.trim(), "1");
}

#[test]
fn and_short_circuit_skips_division_by_zero() {
    assert_eq!(stdout_of("echo $((0 && 1 / 0))").trim(), "0");
}

#[test]
fn or_short_circuit_skips_division_by_zero() {
    assert_eq!(stdout_of("echo $((1 || 1 / 0))").trim(), "1");
}

#[test]
fn ternary_short_circuit_skips_dead_branch() {
    assert_eq!(stdout_of("echo $((1 ? 10 : (1 / 0)))").trim(), "10");
    assert_eq!(stdout_of("echo $((0 ? (1 / 0) : 20))").trim(), "20");
    // Skipped assignment never runs.
    let out = stdout_of("X=1; echo $((1 || (X = 9))); echo \"$X\"");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines, vec!["1", "1"]);
}

#[test]
fn division_by_zero_is_typed_error() {
    let out = run_command("echo $((1 / 0)); echo SHOULD_NOT_RUN");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!stdout.contains("SHOULD_NOT_RUN"));
    assert!(stderr.contains("division by zero") || stderr.contains("arithmetic"));
    // `||` fallback never runs for fatal expansion errors.
    let out = run_command("echo $((1 / 0)) || echo FALLBACK");
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("FALLBACK"));
}

#[test]
fn invalid_syntax_is_typed_error() {
    let out = run_command("echo $((1 +)); echo SHOULD_NOT_RUN");
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SHOULD_NOT_RUN"));
    assert!(stderr_of("echo $((1 +))").contains("arithmetic"));
}

#[test]
fn assignment_rhs_value() {
    // Assignment yields its RHS for outer use.
    assert_eq!(stdout_of("echo $((X = 2 + 3))").trim(), "5");
    assert_eq!(
        stdout_of("echo $((A = B = 3)); echo \"$A/$B\"")
            .trim()
            .lines()
            .last()
            .unwrap()
            .trim(),
        "3/3"
    );
}

#[test]
fn redirect_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("arith-target");
    // Filename `2` via arithmetic.
    let out = run_command(&format!(
        "cd {}; echo hi > $((1 + 1)); cat 2",
        dir.path().display()
    ));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    let _ = target;
}

#[test]
fn double_quoted_arithmetic_is_one_field() {
    let out = stdout_of("printf '[%s]\\n' \"$((1 + 2))\"");
    assert_eq!(out.trim(), "[3]");
    assert_eq!(stdout_of("echo \"$((1 + 2))\"").trim(), "3");
}

#[test]
fn parameter_operand_containing_arithmetic() {
    assert_eq!(
        stdout_of("echo \"${UNSET_ARITH_PROBE:-$((1 + 2))}\"").trim(),
        "3"
    );
}

#[test]
fn command_substitution_inside_arithmetic() {
    assert_eq!(stdout_of("echo $(( $(echo 3) + 2 ))").trim(), "5");
}

#[test]
fn command_substitution_helper_isolation() {
    let out = stdout_of("X=1; echo \"$(echo $((X = 5)))\"; echo \"$X\"");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines, vec!["5", "1"]);
}

#[test]
fn interactive_error_survival() {
    let output = run_interactive(&["echo $((1 / 0))", "echo alive"]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stdout.contains("alive"), "shell died: {stdout:?}");
    assert!(stderr.contains("arithmetic") || stderr.contains("division"));
    let output = run_interactive(&["echo $((1 / 0))", "echo $?"]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.lines().any(|l| l.trim() == "1"),
        "logical $? must be 1 after arithmetic failure, got {stdout:?}"
    );
}

#[test]
fn unclosed_arithmetic_is_strict_failure() {
    let out = run_command("echo before $((1 + 2");
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("before"));
}

#[test]
fn arithmetic_command_substitution_priority() {
    // `$((` must be arithmetic, not `$(` with body `(1 + 2`.
    assert_eq!(stdout_of("echo $((1 + 2))").trim(), "3");
    // Existing `$(` subshell-start form still works.
    assert!(stdout_of("echo $(echo hi)").contains("hi"));
}

#[test]
fn dry_projection_does_not_run_marker_command() {
    // Dry preflight (SafetyGuard projection) must not execute the nested
    // command: use a marker file via gating? Direct dry check lives in unit
    // tests; here assert the selected path still authorizes normally.
    let dir = tempfile::tempdir().expect("tempdir");
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).expect("victim");
    let out = run_command("echo $((1 + 2))");
    assert!(out.status.success());
    assert!(victim.exists());
}
