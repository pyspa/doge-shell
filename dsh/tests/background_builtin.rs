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
    let output = common::run_command("jobs &");
    assert!(
        !output.status.success(),
        "session-bound background builtin unexpectedly succeeded: {:?}",
        output
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot run in background"),
        "clear refusal missing from stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn terminal_bound_builtins_are_refused_before_handler_in_background() {
    // The policy gate runs before the helper spawns, so these never reach
    // their Skim/TUI/editor handlers: no hang, no UI, non-zero exit, and
    // the shared refusal diagnostic on stderr.
    for name in ["dashboard", "gco", "procs", "gwt", "timing"] {
        let output = common::run_command(&format!("{name} &"));
        assert!(
            !output.status.success(),
            "{name} in background unexpectedly succeeded: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cannot run in background"),
            "{name}: clear refusal missing from stderr: {output:?}"
        );
    }
}

#[test]
fn pipeline_with_background_builtin_stage_works() {
    let output = common::run_command("dirs | cat & ; sleep 2");
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
/// Non-interactive mode waits for background jobs synchronously at launch,
/// so the helper is guaranteed done before `jobs` runs — no sleep-polling
/// flakiness by design.
#[test]
fn background_reexec_builtin_leaves_job_table_after_completion() {
    let output = common::run_command("dirs & ; jobs");
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
    let output = common::run_command("dirs | cat & ; jobs");
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("there are no jobs"),
        "completed background builtin pipeline still in job table: {stdout:?}"
    );
}
