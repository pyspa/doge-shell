//! Phase 2: background builtins run through `posix_spawn` + re-exec, never
//! a `fork()` child. The helper inherits the parent snapshot and the
//! already-materialized argv; its mutations stay inside the helper.

mod common;

use std::fs;

#[test]
fn background_sync_builtin_runs_through_reexec() {
    // `dirs` is a builtin: its output can only reach the file if the helper
    // spawned, read the request, and ran the handler on fds the parent wired.
    let out = tempfile::NamedTempFile::new().expect("temp output");
    let path = out.path().to_path_buf();
    drop(out);
    let status = common::run_command(&format!("dirs > {} & sleep 2", path.display()));
    assert!(status.status.success(), "command failed: {:?}", status);
    let written = fs::read_to_string(&path).expect("read helper output");
    assert!(
        !written.trim().is_empty(),
        "background builtin produced no output: {written:?}"
    );
    fs::remove_file(&path).ok();
}

#[test]
fn background_async_builtin_uses_async_handler() {
    // `comp-gen` is an async builtin; `--help` is served by the async
    // implementation without touching the network or the filesystem.
    let output = common::run_command("comp-gen --help & sleep 3");
    assert!(output.status.success(), "command failed: {:?}", output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Usage: comp-gen"),
        "async builtin output missing: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn background_builtin_stdout_reaches_terminal() {
    let output = common::run_command("dirs & sleep 2");
    assert!(output.status.success(), "command failed: {:?}", output);
    assert!(
        !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "background builtin stdout missing: {:?}",
        output
    );
}

#[test]
fn background_cd_does_not_change_parent_cwd() {
    let cwd = std::env::current_dir().expect("cwd");
    let output = common::run_command("cd /tmp & sleep 1; pwd");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(cwd.to_string_lossy().as_ref()),
        "parent cwd changed by background cd: {stdout:?}"
    );
}

#[test]
fn background_export_does_not_leak_into_parent() {
    let output = common::run_command("export DSH_BG_ONLY_MARKER=xyz & sleep 1; echo done");
    assert!(output.status.success(), "command failed: {:?}", output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("done"),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        std::env::var("DSH_BG_ONLY_MARKER").is_err(),
        "background export leaked into the test process environment"
    );
}

#[test]
fn session_bound_builtin_fails_clearly_in_background() {
    // `jobs &` is an async AND-OR list: the parent launch succeeds (exit 0)
    // while the helper refuses the session-bound builtin inside itself.
    // The refusal inherits the caller stderr directly (no drain).
    let output = common::run_command("jobs & echo AFTER");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("AFTER"),
        "parent line aborted by background refusal: {stdout:?}"
    );
    // Non-interactive background helpers inherit the caller fds directly
    // (no capture monitor), so the helper diagnostic lands on stderr.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot run in background"),
        "clear refusal missing from background stderr: {stderr:?}"
    );
}

#[test]
fn terminal_bound_builtins_are_refused_before_handler_in_background() {
    // The policy gate runs for the selected builtin inside the helper, so
    // these never reach their Skim/TUI/editor handlers: no hang, no UI.
    // The parent launch itself succeeds; the refusal surfaces on stderr.
    for name in ["dashboard", "gco", "procs", "gwt", "timing"] {
        let output = common::run_command(&format!("{name} & echo AFTER"));
        assert!(
            output.status.success(),
            "{name} in background unexpectedly failed the parent line: {output:?}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("AFTER"),
            "{name}: parent line aborted by background refusal: {output:?}"
        );
        // Like every other non-interactive background stream, the helper
        // refusal inherits the caller fds: it surfaces on stderr, never on
        // the parent's stdout.
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cannot run in background"),
            "{name}: clear refusal missing from background stderr: {stderr:?}"
        );
    }
}

#[test]
fn pipeline_with_background_builtin_stage_works() {
    let output = common::run_command("dirs | cat & sleep 2");
    assert!(output.status.success(), "command failed: {:?}", output);
    assert!(
        !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "pipelined background builtin produced no output: {:?}",
        output
    );
}

/// A completed background re-exec builtin must leave the job table: the
/// helper's exit flows through `waitpid` into the canonical tree, and `jobs`
/// reconciles (then drops) completed jobs before listing.
///
/// An async launch never waits, so `jobs` runs after a foreground `sleep`
/// that outlives the fast helper — no sleep-polling flakiness by design.
#[test]
fn background_reexec_builtin_leaves_job_table_after_completion() {
    let output = common::run_command("dirs & sleep 1; jobs");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("there are no jobs"),
        "completed background builtin still in job table: {stdout:?}"
    );
}

/// The mixed builtin-helper → external pipeline must drain as one lifecycle:
/// builtin self-wait, Completed-head traversal, external wait, and strict
/// tree completion together empty the table.
#[test]
fn completed_background_builtin_pipeline_leaves_job_table() {
    let output = common::run_command("dirs | cat & sleep 1; jobs");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("there are no jobs"),
        "completed background builtin pipeline still in job table: {stdout:?}"
    );
}

/// `bg` with no stopped job is an error, never success.
#[test]
fn bg_without_stopped_job_reports_error() {
    let output = common::run_command("bg; echo BG:$?");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("BG:1"),
        "bg without a stopped job must report status 1: {stdout:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bg:"),
        "bg failure needs a diagnostic: {stderr:?}"
    );
}

/// `jobs` rejects unknown options instead of silently ignoring argv.
#[test]
fn jobs_invalid_option_reports_error() {
    let output = common::run_command("jobs --definitely-invalid; echo JOBS:$?");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("JOBS:1"),
        "jobs with an invalid option must report status 1: {stdout:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("jobs:"),
        "jobs failure needs a diagnostic: {stderr:?}"
    );
}

/// `jobs -p` emits raw numeric process-group IDs only: the launched async
/// job's associated PID shows up as an integer line with no table header.
/// The trailing `wait` reaps the child so no process leaks from the test.
#[test]
fn jobs_pgid_lists_active_job_process_group() {
    let output = common::run_command("sleep 1 & echo ASYNC:$!; jobs -p; wait $!");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let async_pid: i32 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("ASYNC:"))
        .expect("missing ASYNC pid line")
        .trim()
        .parse()
        .expect("async pid must be numeric");
    let numeric_lines: Vec<i32> = stdout
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .collect();
    assert!(
        numeric_lines.contains(&async_pid),
        "jobs -p must list the async job pgid {async_pid}: {stdout:?}"
    );
    // Machine-readable shape: every line is either the ASYNC marker or a raw
    // integer (no table header, no prose). Substring checks would be brittle
    // against future shell notices, so the line shape itself is the contract.
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        assert!(
            trimmed.strip_prefix("ASYNC:").is_some() || trimmed.parse::<i32>().is_ok(),
            "unexpected jobs -p line {line:?} in {stdout:?}"
        );
    }
}
