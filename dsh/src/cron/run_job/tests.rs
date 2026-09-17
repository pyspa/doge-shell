use super::*;
use dsh_types::cron::job::RunTrigger;
use dsh_types::schedule::NotifyPolicy;
use std::collections::HashMap;

fn run(command: &str) -> ClaimedRun {
    ClaimedRun {
        run_id: "00000000-0000-4000-8000-000000000000".to_string(),
        job_id: 1,
        job_name: "probe".to_string(),
        kind: JobKind::Sh,
        command: command.to_string(),
        agent: None,
        cwd: "/tmp".to_string(),
        env: HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        timeout_secs: 10,
        notify: NotifyPolicy::default(),
        scheduled_for: 0,
        trigger: RunTrigger::Tick,
        last_digest: None,
    }
}

#[test]
fn a_successful_command_is_a_successful_run() {
    let outcome = shell_outcome(&run("echo hello"));
    assert_eq!(outcome.state, RunState::Succeeded);
    assert_eq!(outcome.reason, None);
    assert_eq!(outcome.exit_code, 0);
    assert!(outcome.digest.is_some());
}

#[test]
fn a_nonzero_exit_is_a_failed_run() {
    let outcome = shell_outcome(&run("exit 2"));
    assert_eq!(outcome.state, RunState::Failed);
    assert_eq!(outcome.exit_code, 2);
    // Not a settled reason: the next tick is worth trying.
    assert_eq!(outcome.reason, None);
}

#[test]
fn a_timeout_is_recorded_as_one() {
    let mut spec = run("sleep 30");
    spec.timeout_secs = 1;
    let outcome = shell_outcome(&spec);
    assert_eq!(outcome.state, RunState::Failed);
    assert_eq!(outcome.reason, Some(RunReason::Timeout));
    assert!(outcome.timed_out);
}

/// A job's output is persisted for as long as its history is, so a command
/// that prints a token must not leave it on disk.
#[test]
fn output_is_masked_before_it_is_stored() {
    let outcome = shell_outcome(&run(
        "echo 'AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCY'",
    ));
    assert!(
        !outcome.stdout.contains("wJalrXUtnFEMIK7MDENGbPxRfiCY"),
        "{}",
        outcome.stdout
    );
}

/// A run that produced no output must leave the previous digest alone, or the
/// next real run would look like a change when it is not.
#[test]
fn a_stopped_run_does_not_move_the_change_baseline() {
    let outcome = stopped(
        RunState::Skipped,
        RunReason::StillRunning,
        "a previous run is still going",
        Instant::now(),
    );
    assert_eq!(outcome.digest, None);
    assert_eq!(outcome.state, RunState::Skipped);
    assert_eq!(outcome.reason, Some(RunReason::StillRunning));
}
