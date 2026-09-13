use super::*;
use dsh_types::cron::job::{JobKind, RunTrigger};
use dsh_types::schedule::NotifyPolicy;
use std::collections::HashMap;

fn run(command: &str, timeout_secs: u64) -> ClaimedRun {
    ClaimedRun {
        run_id: "00000000-0000-4000-8000-000000000000".to_string(),
        job_id: 1,
        job_name: "probe".to_string(),
        kind: JobKind::Sh,
        command: command.to_string(),
        agent: None,
        cwd: "/tmp".to_string(),
        env: HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        timeout_secs,
        notify: NotifyPolicy::default(),
        scheduled_for: 0,
        trigger: RunTrigger::Tick,
        last_digest: None,
    }
}

#[test]
fn a_command_reports_its_output_and_status() {
    let outcome = run_command(&run("echo hello", 10));
    assert_eq!(outcome.stdout.trim(), "hello");
    assert_eq!(outcome.exit_code, 0);
    assert!(!outcome.timed_out);
}

#[test]
fn a_failing_command_keeps_its_exit_code() {
    let outcome = run_command(&run("exit 3", 10));
    assert_eq!(outcome.exit_code, 3);
    assert!(!outcome.timed_out);
}

#[test]
fn stderr_is_captured_separately() {
    let outcome = run_command(&run("echo oops >&2", 10));
    assert_eq!(outcome.stderr.trim(), "oops");
    assert!(outcome.stdout.is_empty());
}

/// Reading the pipes after waiting would deadlock here, which is exactly the
/// kind of job someone schedules.
#[test]
fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
    let outcome = run_command(&run("seq 1 200000", 30));
    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome.stdout.lines().count() == 200_000,
        "{}",
        outcome.stdout.len()
    );
}

#[test]
fn a_command_that_overruns_is_killed() {
    let outcome = run_command(&run("sleep 30", 1));
    assert!(outcome.timed_out);
    assert_eq!(outcome.exit_code, TIMED_OUT);
    assert!(outcome.duration.as_secs() < 10, "{:?}", outcome.duration);
}

/// The whole pipeline goes, not just the `sh` that started it.
#[test]
fn a_timeout_takes_the_whole_process_group() {
    let outcome = run_command(&run("sleep 30 & sleep 30", 1));
    assert!(outcome.timed_out);
}

#[test]
fn a_command_that_cannot_start_is_reported_not_panicked() {
    let mut spec = run("true", 10);
    spec.cwd = "/definitely/not/a/directory".to_string();
    let outcome = run_command(&spec);
    assert_eq!(outcome.exit_code, SPAWN_FAILED);
    assert!(
        outcome.stderr.contains("failed to start"),
        "{}",
        outcome.stderr
    );
}

/// A job runs with the environment it was registered with, not the one the
/// process that happened to tick it is carrying.
#[test]
fn the_environment_is_the_jobs_own() {
    unsafe { std::env::set_var("DSH_CRON_TEST_LEAK", "leaked") };
    let outcome = run_command(&run("echo \"[$DSH_CRON_TEST_LEAK]\"", 10));
    unsafe { std::env::remove_var("DSH_CRON_TEST_LEAK") };
    assert_eq!(outcome.stdout.trim(), "[]");
}

/// The digest is persisted, so it has to mean the same thing after a compiler
/// upgrade. A fixed value is the only way to notice if that ever changes.
#[test]
fn the_digest_is_stable_across_builds() {
    assert_eq!(digest("hello\n"), 12230792304413059251_u64);
}

#[test]
fn the_digest_ignores_colour_and_trailing_space() {
    assert_eq!(digest("hello\n"), digest("hello   \n"));
    assert_eq!(digest("hello\n"), digest("\u{1b}[31mhello\u{1b}[0m\n"));
    assert_ne!(digest("hello\n"), digest("goodbye\n"));
    assert_ne!(digest("a\nb\n"), digest("ab\n"));
}

#[test]
fn a_preview_is_the_first_useful_line() {
    assert_eq!(preview("\n\n  hello  \nworld\n"), "hello");
    assert_eq!(preview(""), "");
    assert_eq!(preview("\u{1b}[31mred\u{1b}[0m"), "red");
    let long = "x".repeat(500);
    let preview = preview(&long);
    assert_eq!(preview.chars().count(), PREVIEW_CHARS);
    assert!(preview.ends_with('…'));
}

/// A corrupt row must not be able to put anything but a UUID on a command line.
#[test]
fn only_a_uuid_can_start_a_run_child() {
    for bad in ["", "not-a-uuid", "; rm -rf /", "../../etc/passwd"] {
        assert!(spawn_run_child(bad).is_err(), "{bad}");
    }
}
