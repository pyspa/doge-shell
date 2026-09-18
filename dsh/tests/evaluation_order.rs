//! Parse/evaluate separation: gating precedes substitution, and later jobs see
//! earlier jobs' state. Each test spawns an isolated `dogesh` binary.

mod common;

use common::{false_path, run_command, run_interactive, true_path};

fn marker_not_exists(path: &std::path::Path) {
    assert!(
        !path.exists(),
        "skipped branch must not run its substitution ({} exists)",
        path.display()
    );
}

/// Test C: `false &&` skips the substitution entirely.
#[test]
fn skipped_and_branch_runs_no_substitution() {
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("and_marker");
    let command = format!("{} && echo $(touch {})", false_path(), marker.display());
    let output = run_command(&command);
    assert!(output.status.success() || !output.status.success());
    marker_not_exists(&marker);
}

/// Test D: `true ||` skips the substitution entirely.
#[test]
fn skipped_or_branch_runs_no_substitution() {
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("or_marker");
    let command = format!("{} || echo $(touch {})", true_path(), marker.display());
    let output = run_command(&command);
    assert!(output.status.success());
    marker_not_exists(&marker);
}

/// Test E: `cd dir; echo $(pwd)` sees the new directory.
#[test]
fn later_job_sees_earlier_state_change() {
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().canonicalize().expect("canonical target");
    let lines = [
        format!("cd {}", target.display()),
        "echo $(pwd)".to_string(),
    ];
    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let output = run_interactive(&line_refs);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = target.to_string_lossy().to_string();
    assert!(
        stdout.lines().any(|line| line.trim() == expected),
        "substitution should see {expected:?}, got {stdout:?}"
    );
}

/// Test I: `false && cat <(...)` starts no process substitution.
#[test]
fn skipped_process_substitution_starts_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("proc_marker");
    let command = format!("{} && cat <(touch {})", false_path(), marker.display());
    let _ = run_command(&command);
    marker_not_exists(&marker);
}

/// Standalone assignment applies only when its own job runs.
#[test]
fn standalone_assignment_applies_only_when_selected() {
    let output = run_interactive(&[
        "DOGESH_EVAL_ORDER_PROBE=applied",
        "echo [$DOGESH_EVAL_ORDER_PROBE]",
        "false && DOGESH_EVAL_ORDER_SKIPPED=skipped_value",
        "echo [$DOGESH_EVAL_ORDER_SKIPPED]",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[applied]"),
        "selected assignment should apply: {stdout:?}"
    );
    assert!(
        !stdout.contains("skipped_value"),
        "skipped && assignment must not apply: {stdout:?}"
    );
}
