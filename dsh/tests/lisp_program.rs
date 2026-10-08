//! Lisp program/script runtime: `-l` programs, `dogesh lisp FILE`, and
//! `lisp` builtin error propagation (binary-level contract).
mod common;

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(15);

fn stdout_lines(output: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

fn write_script(dir: &tempfile::TempDir, name: &str, body: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn inline_program_evaluates_every_top_level_form() {
    let output = common::run_dsh(["-l", "(print \"first\") (+ 20 22)"], TIMEOUT);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout_lines(&output), vec!["first", "42"]);
}

#[test]
fn inline_program_runtime_error_is_a_failure() {
    let output = common::run_dsh(["-l", "(does-not-exist)"], TIMEOUT);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does-not-exist"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lisp_builtin_runtime_error_is_a_failure() {
    let output = common::run_dsh(["-c", "lisp '(does-not-exist)'"], TIMEOUT);
    assert!(
        !output.status.success(),
        "lisp evaluation failure must be a non-zero status: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("lisp:"), "stderr:\n{stderr}");
    assert_eq!(
        stderr.matches("does-not-exist").count(),
        1,
        "the Lisp error must be reported exactly once, stderr:\n{stderr}"
    );
}

#[test]
fn lisp_file_evaluates_every_top_level_form() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(&dir, "script.lisp", "(print \"first\")\n(+ 20 22)\n");
    let output = common::run_dsh(["lisp", script.as_str()], TIMEOUT);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout_lines(&output), vec!["first", "42"]);
}

#[test]
fn lisp_file_receives_argv0_and_argv() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "args.lisp",
        "(print *argv0*)\n(print (nth 0 *argv*))\n(nth 1 *argv*)\n",
    );
    let output = common::run_dsh(["lisp", script.as_str(), "--", "alpha", "--flag"], TIMEOUT);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout_lines(&output),
        vec![script, "alpha".to_string(), "--flag".to_string()]
    );
}

#[test]
fn lisp_file_reports_a_missing_file_with_its_path() {
    let missing = "/definitely/not/exist.lisp";
    let output = common::run_dsh(["lisp", missing], TIMEOUT);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(missing),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lisp_file_reports_parse_failures_with_path_and_cause() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(&dir, "broken.lisp", "(define x 1)\n)\n");
    let output = common::run_dsh(["lisp", script.as_str()], TIMEOUT);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains(script.as_str()), "stderr:\n{stderr}");
    assert!(stderr.to_lowercase().contains("parse"), "stderr:\n{stderr}");
}

#[test]
fn lisp_file_runtime_error_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(&dir, "fails.lisp", "(define x 1)\n(does-not-exist)\n");
    let output = common::run_dsh(["lisp", script.as_str()], TIMEOUT);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains(script.as_str()), "stderr:\n{stderr}");
    assert!(stderr.contains("does-not-exist"), "stderr:\n{stderr}");
}
