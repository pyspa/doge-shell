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

#[test]
fn wait_percent_number_waits_job() {
    let failed = stdout_of("false & wait %1; echo STATUS:$?");
    assert!(
        failed.contains("STATUS:1"),
        "wait %1 must report job 1's status: {failed:?}"
    );
    let passed = stdout_of("true & wait %1; echo STATUS:$?");
    assert!(
        passed.contains("STATUS:0"),
        "wait %1 must report job 1's status: {passed:?}"
    );
}

#[test]
fn wait_current_and_previous_aliases() {
    let prev = stdout_of("false & true & wait %-; echo STATUS:$?");
    assert!(
        prev.contains("STATUS:1"),
        "%- must resolve the previous job: {prev:?}"
    );
    let current = stdout_of("false & true & wait %%; echo STATUS:$?");
    assert!(
        current.contains("STATUS:0"),
        "%% must alias the current job: {current:?}"
    );
    let plus = stdout_of("false & true & wait %+; echo STATUS:$?");
    assert!(
        plus.contains("STATUS:0"),
        "%+ must resolve the current job: {plus:?}"
    );
}

#[test]
fn wait_percent_alone_is_current_and_single_previous_alias() {
    let bare = stdout_of("false & true & wait %; echo STATUS:$?");
    assert!(
        bare.contains("STATUS:0"),
        "% must alias the current job: {bare:?}"
    );
    let single = stdout_of("false & wait %-; echo STATUS:$?");
    assert!(
        single.contains("STATUS:1"),
        "single-job %- must resolve to that job: {single:?}"
    );
}

#[test]
fn wait_bare_decimal_is_pid_not_job_number() {
    // Job id 1 exists, but `wait 1` is PID 1 — not our child — so 127.
    let stdout = stdout_of("true & wait 1; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:127"),
        "bare decimals are PIDs, never job numbers: {stdout:?}"
    );
}

#[test]
fn wait_percent_number_survives_jobs_reconciliation() {
    let stdout = stdout_of("false & sleep 1; jobs; wait %1; echo STATUS:$?");
    assert!(
        stdout.contains("there are no jobs"),
        "jobs must have reconciled the table: {stdout:?}"
    );
    assert!(
        stdout.contains("STATUS:1"),
        "explicit %N must serve the retained ledger status: {stdout:?}"
    );
}

#[test]
fn wait_mixed_pid_and_jobspec_returns_last_status() {
    let forward = stdout_of("false & p1=$!; true & p2=$!; wait %1 $p2; echo STATUS:$?");
    assert!(
        forward.contains("STATUS:0"),
        "last operand wins (jobspec, PID): {forward:?}"
    );
    let reverse = stdout_of("false & p1=$!; true & p2=$!; wait $p2 %1; echo STATUS:$?");
    assert!(
        reverse.contains("STATUS:1"),
        "last operand wins (PID, jobspec): {reverse:?}"
    );
}

#[test]
fn wait_reports_async_pipeline_tail_via_jobspec() {
    let stdout = stdout_of("false | true & wait %1; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:0"),
        "jobspec pipeline status is the tail status: {stdout:?}"
    );
}

#[test]
fn wait_double_dash_ends_option_parsing() {
    let stdout = stdout_of("wait -- 999999; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:127"),
        "post--- operands are PIDs, not options: {stdout:?}"
    );
}

#[test]
fn wait_accepts_p_option() {
    let stdout = stdout_of("false & p=$!; wait -p done $p; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:1"),
        "wait -p must serve the child status: {stdout:?}"
    );
}

#[test]
fn wait_rejects_f_option() {
    let f = stdout_of("wait -f; echo STATUS:$?");
    assert!(f.contains("STATUS:1"), "wait -f stays a usage error: {f:?}");
}

#[test]
fn wait_next_returns_first_completion_not_first_operand() {
    let stdout = stdout_of(
        // The slow job's stdio goes to /dev/null: its orphaned `sleep`
        // would otherwise hold the harness pipe open for the full 30s.
        "sleep 30 > /dev/null 2>&1 & slow=$!; false & fast=$!; \
         wait -n $slow $fast; echo \"STATUS:$?\"; kill $slow; wait $slow; echo CLEANED",
    );
    assert!(
        stdout.contains("STATUS:1"),
        "wait -n must return the fast failure, not the first operand: {stdout:?}"
    );
}

#[test]
fn wait_next_without_operands_waits_for_any_completion() {
    let stdout = stdout_of("false & true & wait -n; echo STATUS:$?");
    let value = last_line_value(&stdout, "STATUS:");
    assert!(
        value == "0" || value == "1",
        "wait -n serves one real completion, never 127: {stdout:?}"
    );
}

#[test]
fn wait_next_accepts_job_spec_targets() {
    let failed = stdout_of("false & wait -n %1; echo STATUS:$?");
    assert!(
        failed.contains("STATUS:1"),
        "wait -n %1 must serve job 1's status: {failed:?}"
    );
    let mixed = stdout_of("false & p=$!; true & wait -n $p %2; echo STATUS:$?");
    let value = last_line_value(&mixed, "STATUS:");
    assert!(
        value == "0" || value == "1",
        "wait -n mixes PID and jobspec targets: {mixed:?}"
    );
}

#[test]
fn wait_next_without_targets_reports_127_while_bare_wait_reports_0() {
    let stdout = stdout_of("wait -n; echo N:$?; wait; echo BARE:$?");
    assert!(
        stdout.contains("N:127"),
        "wait -n with no known jobs reports 127: {stdout:?}"
    );
    assert!(
        stdout.contains("BARE:0"),
        "bare wait with no known jobs reports 0: {stdout:?}"
    );
}

#[test]
fn wait_next_ignores_unknown_pid_when_valid_target_exists() {
    let stdout = stdout_of("false & p=$!; wait -n 999999 $p; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:1"),
        "an unknown operand must not abort a valid wait-any: {stdout:?}"
    );
}

#[test]
fn wait_next_all_unknown_reports_127() {
    let stdout = stdout_of("wait -n 999998 999999; echo STATUS:$?");
    assert!(
        stdout.contains("STATUS:127"),
        "all-unknown wait-any reports 127: {stdout:?}"
    );
}

#[test]
fn wait_next_consumes_selected_status_once_and_keeps_other() {
    let stdout = stdout_of(
        "false & p1=$!; true & p2=$!; wait -n $p1 $p2; echo FIRST:$?; \
         wait $p1; echo R1:$?; wait $p2; echo R2:$?",
    );
    let first = last_line_value(&stdout, "FIRST:");
    let r1 = last_line_value(&stdout, "R1:");
    let r2 = last_line_value(&stdout, "R2:");
    if first == "1" {
        assert_eq!(r1, "127", "consumed p1 must be gone: {stdout:?}");
        assert_eq!(r2, "0", "unselected p2 must be retained: {stdout:?}");
    } else {
        assert_eq!(first, "0", "wait -n serves one real status: {stdout:?}");
        assert_eq!(r1, "1", "unselected p1 must be retained: {stdout:?}");
        assert_eq!(r2, "127", "consumed p2 must be gone: {stdout:?}");
    }
}

#[test]
fn wait_next_honors_frozen_pipefail_policy() {
    let tail = stdout_of("false | true & p=$!; wait -n $p; echo STATUS:$?");
    assert!(
        tail.contains("STATUS:0"),
        "pipefail OFF serves the tail status: {tail:?}"
    );
    let pipefail = stdout_of("set -o pipefail; false | true & p=$!; wait -n $p; echo STATUS:$?");
    assert!(
        pipefail.contains("STATUS:1"),
        "pipefail ON serves the upstream failure: {pipefail:?}"
    );
}

#[test]
fn fg_reports_status_without_consuming_wait_status() {
    let stdout = stdout_of(
        "false & p=$!; \
         fg %1; echo FG:$?; \
         wait $p; echo WAIT:$?",
    );

    assert!(stdout.contains("FG:1"), "fg must report: {stdout:?}");
    assert!(stdout.contains("WAIT:1"), "wait must still own: {stdout:?}");
}

#[test]
fn fg_pipefail_uses_frozen_policy_without_consuming_wait_status() {
    let stdout = stdout_of(
        "set -o pipefail; false | true & p=$!; \
         fg %1; echo FG:$?; \
         wait $p; echo WAIT:$?",
    );

    assert!(stdout.contains("FG:1"), "fg pipefail ON: {stdout:?}");
    assert!(stdout.contains("WAIT:1"), "wait retains: {stdout:?}");
}

#[test]
fn wait_p_assigns_active_pid() {
    // `$?` must be saved before the reporting `echo`s: every `echo`
    // overwrites it.
    let stdout = stdout_of(
        "false & p=$!; wait -p done $p; rc=$?; echo P:$p; echo DONE:$done; echo STATUS:$rc",
    );
    let pid = last_line_value(&stdout, "P:");
    let done = last_line_value(&stdout, "DONE:");
    let status = last_line_value(&stdout, "STATUS:");
    assert_eq!(status, "1", "child status passes through: {stdout:?}");
    assert_eq!(done, pid, "-p must publish the associated PID: {stdout:?}");
    assert_ne!(pid, "", "$! must expand: {stdout:?}");
}

#[test]
fn wait_p_jobspec_assigns_pid_not_job_number() {
    let stdout = stdout_of(
        "false & p=$!; wait -p done %1; rc=$?; echo P:$p; echo DONE:$done; echo STATUS:$rc",
    );
    let pid = last_line_value(&stdout, "P:");
    let done = last_line_value(&stdout, "DONE:");
    let status = last_line_value(&stdout, "STATUS:");
    assert_eq!(status, "1", "jobspec status passes through: {stdout:?}");
    assert_eq!(done, pid, "%1 must publish the PID, not \"1\": {stdout:?}");
}

#[test]
fn wait_p_real_child_status_127_still_assigns() {
    let stdout = stdout_of(
        "definitely-not-a-command-xyz & p=$!; wait -p done $p; rc=$?; \
         echo P:$p; echo DONE:$done; echo STATUS:$rc",
    );
    let pid = last_line_value(&stdout, "P:");
    let done = last_line_value(&stdout, "DONE:");
    let status = last_line_value(&stdout, "STATUS:");
    assert_eq!(
        status, "127",
        "unknown-command helper exits 127: {stdout:?}"
    );
    assert_eq!(done, pid, "real child 127 must still assign: {stdout:?}");
}

#[test]
fn wait_p_unknown_pid_leaves_variable_unset() {
    let stdout = stdout_of(
        "done=old; wait -p done 999999; rc=$?; \
         test \"$done\" = '$done' && echo DONE-UNSET; echo STATUS:$rc",
    );
    assert!(
        stdout.contains("STATUS:127"),
        "unknown PID reports 127: {stdout:?}"
    );
    assert!(
        stdout.contains("DONE-UNSET"),
        "unknown target must leave -p unset: {stdout:?}"
    );
}

#[test]
fn wait_p_valid_then_unknown_leaves_variable_unset() {
    let stdout = stdout_of(
        "false & p=$!; wait -p done $p 999999; rc=$?; \
         test \"$done\" = '$done' && echo DONE-UNSET; echo STATUS:$rc",
    );
    assert!(
        stdout.contains("STATUS:127"),
        "final unknown target wins: {stdout:?}"
    );
    assert!(
        stdout.contains("DONE-UNSET"),
        "trailing NoCompletion resets the identity: {stdout:?}"
    );
}

#[test]
fn wait_p_unknown_then_valid_assigns_valid_pid() {
    let stdout = stdout_of(
        "false & p=$!; wait -p done 999999 $p; rc=$?; \
         echo P:$p; echo DONE:$done; echo STATUS:$rc",
    );
    let pid = last_line_value(&stdout, "P:");
    let done = last_line_value(&stdout, "DONE:");
    let status = last_line_value(&stdout, "STATUS:");
    assert_eq!(status, "1", "valid completion wins: {stdout:?}");
    assert_eq!(
        done, pid,
        "unknown-then-valid publishes the PID: {stdout:?}"
    );
}

#[test]
fn wait_p_multiple_valid_operands_publish_last_identity() {
    let forward = stdout_of(
        "false & p1=$!; true & p2=$!; wait -p done $p1 $p2; rc=$?; \
         echo P2:$p2; echo DONE:$done; echo STATUS:$rc",
    );
    assert_eq!(last_line_value(&forward, "STATUS:"), "0");
    assert_eq!(
        last_line_value(&forward, "DONE:"),
        last_line_value(&forward, "P2:"),
        "forward publishes the last PID: {forward:?}"
    );
    let reverse = stdout_of(
        "false & p1=$!; true & p2=$!; wait -p done $p2 $p1; rc=$?; \
         echo P1:$p1; echo DONE:$done; echo STATUS:$rc",
    );
    assert_eq!(last_line_value(&reverse, "STATUS:"), "1");
    assert_eq!(
        last_line_value(&reverse, "DONE:"),
        last_line_value(&reverse, "P1:"),
        "reverse publishes the last PID: {reverse:?}"
    );
}

#[test]
fn wait_p_bare_wait_publishes_nothing() {
    let stdout = stdout_of(
        "false & p=$!; done=old; wait -p done; rc=$?; \
         test \"$done\" = '$done' && echo DONE-UNSET; echo STATUS:$rc",
    );
    assert!(
        stdout.contains("STATUS:0"),
        "bare wait -p still reports 0: {stdout:?}"
    );
    assert!(
        stdout.contains("DONE-UNSET"),
        "bare wait -p must not invent a PID: {stdout:?}"
    );
}

#[test]
fn wait_np_assigns_selected_pid_and_keeps_other() {
    let stdout = stdout_of(
        "sleep 30 > /dev/null 2>&1 & slow=$!; false & fast=$!; \
         wait -n -p done $slow $fast; rc=$?; \
         echo FAST:$fast; echo DONE:$done; echo STATUS:$rc; \
         kill $slow; wait $slow; echo CLEANED",
    );
    let fast = last_line_value(&stdout, "FAST:");
    let done = last_line_value(&stdout, "DONE:");
    assert!(
        stdout.contains("STATUS:1"),
        "wait -n serves the fast failure: {stdout:?}"
    );
    assert_eq!(done, fast, "-p must publish the selected PID: {stdout:?}");
    assert!(
        stdout.contains("CLEANED"),
        "unselected target stays waitable: {stdout:?}"
    );
}

#[test]
fn wait_np_without_targets_leaves_variable_unset() {
    let stdout = stdout_of(
        "DONE=old; wait -n -p DONE; rc=$?; \
         test \"$DONE\" = '$DONE' && echo DONE-UNSET; echo STATUS:$rc",
    );
    assert!(
        stdout.contains("STATUS:127"),
        "wait -n with no targets reports 127: {stdout:?}"
    );
    assert!(
        stdout.contains("DONE-UNSET"),
        "no-target wait -n must leave -p unset: {stdout:?}"
    );
}

#[test]
fn wait_p_consumes_selected_status_once() {
    let stdout = stdout_of("false & p=$!; wait -p done $p; echo FIRST:$?; wait $p; echo SECOND:$?");
    assert!(stdout.contains("FIRST:1"), "first wait reports: {stdout:?}");
    assert!(
        stdout.contains("SECOND:127"),
        "-p still consumes exactly once: {stdout:?}"
    );
}

#[test]
fn wait_p_honors_frozen_pipefail_policy() {
    let stdout = stdout_of(
        "set -o pipefail; false | true & p=$!; wait -p done $p; rc=$?; \
         echo P:$p; echo DONE:$done; echo STATUS:$rc",
    );
    assert!(
        stdout.contains("STATUS:1"),
        "pipefail ON serves the upstream failure: {stdout:?}"
    );
    assert_eq!(
        last_line_value(&stdout, "DONE:"),
        last_line_value(&stdout, "P:"),
        "identity follows the logical status: {stdout:?}"
    );
}
