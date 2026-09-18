mod common;

use common::{false_path, head_path, run_interactive, true_path, yes_path};
use std::os::unix::fs::PermissionsExt;

fn write_executable_script(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).expect("write helper script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat helper script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod helper script");
    path
}

fn survivor_script(
    dir: &tempfile::TempDir,
    marker_name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let marker = dir.path().join(marker_name);
    let script = write_executable_script(
        dir,
        &format!("survivor_{marker_name}.sh"),
        &format!(
            "#!/bin/sh\nsleep 0.2\nprintf survived > \"{}\"",
            marker.display()
        ),
    );
    (script, marker)
}

/// A pipeline stage completing non-zero does not terminate its siblings.
#[test]
fn left_stage_failure_does_not_kill_right_stage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (script, marker) = survivor_script(&dir, "marker_a");
    let line = format!(
        "{} | {} {}",
        false_path(),
        script.display(),
        marker.display()
    );
    let output = run_interactive(&[line.as_str()]);
    assert!(
        output.status.success(),
        "pipeline should complete successfully. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = std::fs::read_to_string(&marker).unwrap_or_else(|_| {
        panic!(
            "right-hand stage must survive left failure; marker missing: {}",
            marker.display()
        )
    });
    assert_eq!(content, "survived");
}

/// Same invariant for an arbitrary non-zero left status, not just 0/1.
#[test]
fn arbitrary_nonzero_left_stage_does_not_kill_right_stage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let exit7 = write_executable_script(&dir, "exit7.sh", "#!/bin/sh\nexit 7\n");
    let (script, marker) = survivor_script(&dir, "marker_c");
    let line = format!(
        "{} | {} {}",
        exit7.display(),
        script.display(),
        marker.display()
    );
    let output = run_interactive(&[line.as_str()]);
    assert!(
        output.status.success(),
        "pipeline should complete successfully. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = std::fs::read_to_string(&marker).unwrap_or_else(|_| {
        panic!(
            "right-hand stage must survive exit-7 left stage; marker missing: {}",
            marker.display()
        )
    });
    assert_eq!(content, "survived");
}

/// Default pipeline status is the last stage's status (no pipefail).
#[test]
fn pipeline_status_is_last_stage_status() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let exit7 = write_executable_script(&dir, "exit7_status.sh", "#!/bin/sh\nexit 7\n");
    let output = run_interactive(&[
        format!("{} | {}", false_path(), true_path()).as_str(),
        "echo PIPELINE_A:$?",
        format!("{} | {}", true_path(), false_path()).as_str(),
        "echo PIPELINE_B:$?",
        format!("{} | {}", exit7.display(), true_path()).as_str(),
        "echo PIPELINE_C:$?",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("PIPELINE_A:0"),
        "false | true must report 0. Output:\n{}",
        stdout
    );
    assert!(
        stdout.lines().any(|line| line.trim() == "PIPELINE_B:1"),
        "true | false must report non-zero (1). Output:\n{}",
        stdout
    );
    assert!(
        stdout.contains("PIPELINE_C:0"),
        "exit-7 | true must report last stage (0). Output:\n{}",
        stdout
    );
}

/// Consumer-terminated cleanup (`yes | head`) must keep working after the
/// non-zero kill removal: the shell must return promptly.
#[test]
fn consumer_termination_cleanup_still_works() {
    let line = format!("{} | {} -n 1", yes_path(), head_path());
    let output = run_interactive(&[line.as_str()]);
    assert!(
        output.status.success(),
        "yes | head -n 1 must finish without hanging. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "y"),
        "expected at least one line of `yes` output. Output:\n{}",
        stdout
    );
}

/// A process killed by signal N has shell status 128 + N, end to end.
#[test]
fn signal_termination_maps_to_128_plus_signal() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let term = write_executable_script(&dir, "sigterm.sh", "#!/bin/sh\nkill -TERM $$\n");
    let int = write_executable_script(&dir, "sigint.sh", "#!/bin/sh\nkill -INT $$\n");
    let kill = write_executable_script(&dir, "sigkill.sh", "#!/bin/sh\nkill -KILL $$\n");

    let term_out = common::run_command(term.to_str().expect("utf8 path"));
    assert_eq!(
        term_out.status.code(),
        Some(143),
        "SIGTERM self-kill must exit 143. stderr:\n{}",
        String::from_utf8_lossy(&term_out.stderr)
    );
    let int_out = common::run_command(int.to_str().expect("utf8 path"));
    assert_eq!(
        int_out.status.code(),
        Some(130),
        "SIGINT self-kill must exit 130. stderr:\n{}",
        String::from_utf8_lossy(&int_out.stderr)
    );
    let kill_out = common::run_command(kill.to_str().expect("utf8 path"));
    assert_eq!(
        kill_out.status.code(),
        Some(137),
        "SIGKILL self-kill must exit 137. stderr:\n{}",
        String::from_utf8_lossy(&kill_out.stderr)
    );
}

/// `$?` on the following line reflects the normalized signal status.
#[test]
fn dollar_question_reflects_normalized_signal_status() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let term = write_executable_script(&dir, "sigterm_q.sh", "#!/bin/sh\nkill -TERM $$\n");
    let line = term.to_str().expect("utf8 path").to_string();
    let output = run_interactive(&[line.as_str(), "echo STATUS:$?"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("STATUS:143"),
        "expected STATUS:143 after SIGTERM. Output:\n{}",
        stdout
    );
}

/// The `|>` capture path reports the same normalized status.
#[test]
fn capture_path_reports_normalized_signal_status() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let term = write_executable_script(&dir, "sigterm_cap.sh", "#!/bin/sh\nkill -TERM $$\n");
    let line = format!("{} |>", term.display());
    let output = run_interactive(&[line.as_str(), "echo CAPSTATUS:$?"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("CAPSTATUS:143"),
        "expected CAPSTATUS:143 via execute_with_capture. Output:\n{}",
        stdout
    );
}
