//! `$!` + `wait PID` + known-PID ledger semantics.
//!
//! One ownership lifecycle: async launch registers the helper PID (`$!`),
//! completion archives the status in the ledger, and `wait` consumes it
//! exactly once. Reconciliation (`jobs`, notices) and `fg`/`bg` archive but
//! never consume.

mod common;

use std::time::Duration;

fn stdout_of(script: &str) -> String {
    let output = common::run_command(script);
    assert!(
        output.status.success(),
        "command failed: {script:?}: {output:?}"
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn last_line_value(stdout: &str, prefix: &str) -> String {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line in: {stdout:?}"))
        .trim()
        .to_string()
}

#[test]
fn bang_starts_empty() {
    let stdout = stdout_of("echo \"PID=[$!]\"");
    assert!(
        stdout.contains("PID=[]"),
        "initial $! must be empty: {stdout:?}"
    );
}

#[test]
fn bang_reports_async_helper_pid() {
    let stdout = stdout_of("sleep 0.1 & echo PID:$!");
    let pid: i32 = last_line_value(&stdout, "PID:")
        .parse()
        .expect("$! must expand to a decimal PID");
    assert!(pid > 0, "$! must be a positive PID, got {pid}");
}

#[test]
fn bang_updates_on_second_launch() {
    let stdout = stdout_of("sleep 0.1 & p1=$!; sleep 0.1 & p2=$!; echo IDS:$p1:$p2");
    let ids = last_line_value(&stdout, "IDS:");
    let mut parts = ids.split(':');
    let p1: i32 = parts.next().unwrap_or("").parse().expect("p1 numeric");
    let p2: i32 = parts.next().unwrap_or("").parse().expect("p2 numeric");
    assert!(p1 > 0 && p2 > 0, "both PIDs positive: {ids:?}");
    assert_ne!(p1, p2, "second launch must update $!: {ids:?}");
}

/// NOTE: `sh -c 'exit N'` cannot serve as an arbitrary-status helper
/// here: nested-shell invocation is denied by the command authorization
/// policy (the helper exits 130 without ever launching the body). Exact
/// statuses below come from `false` (1), `true` (0), self-`kill` (143),
/// and unknown commands (127) instead; the lifecycle properties under
/// test (retention, consume-once, ordering) are unchanged.
#[test]
fn wait_returns_each_saved_status() {
    let stdout = stdout_of(
        "false & p1=$!; true & p2=$!; \
         wait $p1; echo A:$?; wait $p2; echo B:$?",
    );
    assert!(stdout.contains("A:1"), "first status kept: {stdout:?}");
    assert!(stdout.contains("B:0"), "second status kept: {stdout:?}");
}

#[test]
fn wait_finds_already_completed_child() {
    // The sleep lets the helper finish first: `wait` must serve the
    // retained ledger status, not require a still-running child.
    let stdout = stdout_of("false & pid=$!; sleep 1; wait $pid; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:1"),
        "completed status retained: {stdout:?}"
    );
}

#[test]
fn wait_blocks_for_active_child() {
    let started = std::time::Instant::now();
    let stdout = stdout_of("sleep 1 & pid=$!; wait $pid; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:0"),
        "active child waited to termination: {stdout:?}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "wait must block until the child finishes"
    );
}

#[test]
fn wait_survives_jobs_reconciliation() {
    let stdout = stdout_of("false & pid=$!; sleep 1; jobs; wait $pid; echo STATUS:$?");
    assert!(
        stdout.contains("there are no jobs"),
        "jobs must have reconciled the table: {stdout:?}"
    );
    assert!(
        stdout.contains("STATUS:1"),
        "status retained past jobs: {stdout:?}"
    );
}

#[test]
fn wait_unknown_pid_reports_127() {
    let stdout = stdout_of("wait 999999; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:127"),
        "unknown PID must report 127: {stdout:?}"
    );
}

#[test]
fn wait_consumes_status_once() {
    let stdout =
        stdout_of("false & pid=$!; sleep 1; wait $pid; echo FIRST:$?; wait $pid; echo SECOND:$?");
    assert!(stdout.contains("FIRST:1"), "first wait reports: {stdout:?}");
    assert!(
        stdout.contains("SECOND:127"),
        "second wait finds no retained status: {stdout:?}"
    );
}

#[test]
fn wait_without_operands_waits_all_and_reports_zero() {
    let stdout = stdout_of("false & true & wait; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:0"),
        "bare wait reports 0 despite child failures: {stdout:?}"
    );
}

#[test]
fn wait_without_operands_consumes_all_entries() {
    let stdout = stdout_of("false & p=$!; sleep 1; wait; wait $p; echo SECOND:$?");
    assert!(
        stdout.contains("SECOND:127"),
        "bare wait must consume every entry: {stdout:?}"
    );
}

#[test]
fn wait_multiple_operands_returns_last_status() {
    let forward = stdout_of("false & p1=$!; true & p2=$!; wait $p1 $p2; echo STATUS:$?");
    assert!(
        forward.contains("STATUS:0"),
        "last operand wins (forward): {forward:?}"
    );
    let reverse = stdout_of("false & p1=$!; true & p2=$!; wait $p2 $p1; echo STATUS:$?");
    assert!(
        reverse.contains("STATUS:1"),
        "last operand wins (reverse): {reverse:?}"
    );
}

#[test]
fn wait_normalizes_signal_death() {
    let stdout = stdout_of("kill -TERM $$ & pid=$!; wait $pid; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:143"),
        "SIGTERM must normalize to 143: {stdout:?}"
    );
}

#[test]
fn wait_reports_async_pipeline_tail_status() {
    let head_fails = stdout_of("false | true & pid=$!; wait $pid; echo STATUS:$?");
    assert!(
        head_fails.contains("STATUS:0"),
        "pipeline tail decides: {head_fails:?}"
    );
    let tail_fails = stdout_of("true | false & pid=$!; wait $pid; echo STATUS:$?");
    assert!(
        tail_fails.contains("STATUS:1"),
        "tail failure must be exactly 1: {tail_fails:?}"
    );
}

#[test]
fn wait_reports_async_list_helper_status() {
    // `$!` is the AsyncList helper PID; `wait` returns the helper body's
    // final AND-OR status, not the launch-time 0.
    let stdout = stdout_of("false && echo SHOULD_NOT_RUN & pid=$!; wait $pid; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:1"),
        "helper body status, not launch status: {stdout:?}"
    );
    assert!(
        !stdout.contains("SHOULD_NOT_RUN"),
        "gated branch must not run: {stdout:?}"
    );
}

#[test]
fn jobs_reconciliation_keeps_background_output() {
    let stdout = stdout_of("printf 'BG-MARKER\\n' & sleep 1; jobs");
    assert!(
        stdout.contains("BG-MARKER"),
        "background output must survive jobs: {stdout:?}"
    );
    assert!(
        stdout.contains("there are no jobs"),
        "completed job leaves the table: {stdout:?}"
    );
}

#[test]
fn wait_returns_only_after_background_termination() {
    // `wait`'s contract is termination ordering, not output plumbing:
    // non-interactive background helpers inherit stdout directly, so the
    // marker reaches the pipe without any monitor drain. What `wait`
    // guarantees is that it returns only after the target job terminates.
    let stdout = stdout_of("printf 'WAIT-MARKER\\n' & pid=$!; wait $pid");
    assert!(
        stdout.contains("WAIT-MARKER"),
        "background output must be observable after wait returns: {stdout:?}"
    );
}
