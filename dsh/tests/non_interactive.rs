mod common;

use std::time::Duration;

#[test]
fn command_mode_exits_without_waiting_for_interactive_prewarm() {
    let output = common::run_dsh(["-c", "true"], Duration::from_secs(2));
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lisp_mode_exits_after_printing_result() {
    let output = common::run_dsh(["-l", "(+ 1 2)"], Duration::from_secs(2));
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "3");
}

#[test]
fn command_mode_preserves_notebook_setup() {
    let notebook = tempfile::NamedTempFile::new().expect("create notebook path");
    let notebook_path = notebook.path().display().to_string();
    let output = common::run_dsh(
        ["--notebook", notebook_path.as_str(), "-c", "true"],
        Duration::from_secs(2),
    );

    assert!(output.status.success(), "dsh command failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Notebook Mode Active."),
        "combined command/notebook mode skipped notebook setup: {output:?}"
    );
}

#[test]
fn lisp_mode_preserves_notebook_setup() {
    let notebook = tempfile::NamedTempFile::new().expect("create notebook path");
    let notebook_path = notebook.path().display().to_string();
    let output = common::run_dsh(
        ["--notebook", notebook_path.as_str(), "-l", "(+ 1 2)"],
        Duration::from_secs(2),
    );

    assert!(output.status.success(), "dsh lisp failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Notebook Mode Active."));
    assert!(stdout.lines().any(|line| line.trim() == "3"));
}

#[test]
fn timeout_kills_descendants_that_hold_output_pipes() {
    let started = std::time::Instant::now();
    let result = std::panic::catch_unwind(|| {
        common::run_dsh(["-c", "/bin/sleep 30"], Duration::from_millis(100))
    });

    assert!(result.is_err(), "expected the helper to report a timeout");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "timeout handling waited for a descendant process"
    );
}
