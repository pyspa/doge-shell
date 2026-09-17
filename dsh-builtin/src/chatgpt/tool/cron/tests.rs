use super::*;
use crate::agent::AgentRuntime;
use crate::shell_capabilities::AgentTaskStore;
use crate::test_support::{MemoryTaskStore, TestShellProxy, running_task};
use tempfile::tempdir;

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
fn fields_round_trip_including_numbers_as_strings() {
    let request = parse_request(&args(json!({
        "action": "create",
        "job": "digest",
        "max_tokens_per_day": 200000,
        "check": ["out/digest.md exists"],
        "read": ["."],
        "sandbox": true,
    })))
    .unwrap();
    assert_eq!(request.action, Some(CronToolAction::Create));
    assert_eq!(request.job.as_deref(), Some("digest"));
    // A JSON number must survive as the string `cli::parse` expects.
    assert_eq!(request.max_tokens_per_day.as_deref(), Some("200000"));
    assert_eq!(request.check, vec!["out/digest.md exists".to_string()]);
    assert_eq!(request.read, vec![".".to_string()]);
    assert!(request.sandbox);
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

/// A path that fails to resolve (does not exist) must not itself read as
/// "outside the grant" - `apply_grant_option`'s own, much clearer error is
/// what should surface once the call actually reaches it.
#[test]
fn an_unresolvable_path_is_not_reported_as_ungranted() {
    let grant = TaskGrant::default();
    let request = CronToolRequest {
        read: vec!["/definitely/not/a/real/path".to_string()],
        ..Default::default()
    };
    assert!(grant_exceeds_task(&request, &grant).is_none());
}

#[test]
fn a_read_within_the_tasks_write_root_is_allowed() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let grant = TaskGrant {
        write_roots: vec![root.clone()],
        ..TaskGrant::default()
    };
    let request = CronToolRequest {
        read: vec![root.to_string_lossy().into_owned()],
        ..Default::default()
    };
    assert!(grant_exceeds_task(&request, &grant).is_none());
}

#[test]
fn a_write_outside_every_granted_root_is_refused() {
    let granted = tempdir().unwrap();
    let requested = tempdir().unwrap();
    let grant = TaskGrant {
        write_roots: vec![granted.path().canonicalize().unwrap()],
        ..TaskGrant::default()
    };
    let request = CronToolRequest {
        write: vec![requested.path().to_string_lossy().into_owned()],
        ..Default::default()
    };
    let reason = grant_exceeds_task(&request, &grant).expect("should be refused");
    assert!(reason.contains("write"));
}

#[test]
fn every_exact_match_grant_field_is_checked() {
    let grant = TaskGrant {
        commands: vec!["cargo test".to_string()],
        mcp_calls: vec!["mcp:server:tool:{}".to_string()],
        network_hosts: vec!["example.com".to_string()],
        environment: vec!["HOME".to_string()],
        sandbox: false,
        ..TaskGrant::default()
    };

    assert!(
        grant_exceeds_task(
            &CronToolRequest {
                allow_command: vec!["cargo test".to_string()],
                ..Default::default()
            },
            &grant
        )
        .is_none()
    );
    assert!(
        grant_exceeds_task(
            &CronToolRequest {
                allow_command: vec!["rm -rf /".to_string()],
                ..Default::default()
            },
            &grant
        )
        .is_some()
    );
    assert!(
        grant_exceeds_task(
            &CronToolRequest {
                allow_mcp: vec!["mcp:server:other:{}".to_string()],
                ..Default::default()
            },
            &grant
        )
        .is_some()
    );
    assert!(
        grant_exceeds_task(
            &CronToolRequest {
                network: vec!["evil.example".to_string()],
                ..Default::default()
            },
            &grant
        )
        .is_some()
    );
    assert!(
        grant_exceeds_task(
            &CronToolRequest {
                env: vec!["AWS_SECRET_ACCESS_KEY".to_string()],
                ..Default::default()
            },
            &grant
        )
        .is_some()
    );
    assert!(
        grant_exceeds_task(
            &CronToolRequest {
                sandbox: true,
                ..Default::default()
            },
            &grant
        )
        .is_some()
    );
}

/// A call missing a required field must fail before anyone is asked
/// anything, not after - the same ordering `skill_manage`'s own `validate`
/// follows.
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
fn create_an_agent_job_needs_a_goal_not_a_command() {
    let mut proxy = TestShellProxy::default();
    let error = run(
        &args(json!({"action": "create", "schedule": "5m", "agent": true})),
        &mut proxy,
    )
    .unwrap_err();
    assert!(error.contains("goal"), "{error}");
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

fn task_proxy(grant: TaskGrant) -> TestShellProxy {
    let mut task = running_task(std::path::Path::new("/tmp"));
    task.grant = grant;
    let store = std::sync::Arc::new(MemoryTaskStore::default());
    store.save(&task, None).expect("in-memory save");
    TestShellProxy {
        agent_runtime: Some(std::sync::Arc::new(parking_lot::Mutex::new(
            AgentRuntime::new(task, store),
        ))),
        ..Default::default()
    }
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
fn a_grant_wider_than_the_task_is_refused_before_any_confirmation() {
    let granted = tempdir().unwrap();
    let requested = tempdir().unwrap();
    let mut proxy = task_proxy(TaskGrant {
        write_roots: vec![granted.path().canonicalize().unwrap()],
        ..TaskGrant::default()
    });

    let error = run(
        &args(json!({
            "action": "create",
            "schedule": "5m",
            "goal": "do something",
            "agent": true,
            "write": [requested.path().to_string_lossy()],
        })),
        &mut proxy,
    )
    .unwrap_err();

    assert!(error.contains("refused"));
    assert_eq!(proxy.confirm_calls, 0);
    assert!(proxy.cron_tool_calls.is_empty());
}

/// A grant that stays within the task's own is still a write action under an
/// unattended task, so it is still refused - but as a tool-result error the
/// turn works around, not as an `InputRequired` halt: nothing is watching to
/// approve it, and stopping at the first refusal would make every unattended
/// run with a slightly-too-narrow grant wait on a person.
#[test]
fn a_grant_within_the_task_is_refused_without_stopping_once_unattended() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut proxy = task_proxy(TaskGrant {
        write_roots: vec![root.clone()],
        ..TaskGrant::default()
    });

    let error = run(
        &args(json!({
            "action": "create",
            "schedule": "5m",
            "goal": "do something",
            "agent": true,
            "write": [root.to_string_lossy()],
        })),
        &mut proxy,
    )
    .unwrap_err();

    assert!(error.contains("permission required"));
    assert!(proxy.cron_tool_calls.is_empty());
    assert_eq!(
        proxy
            .agent_runtime
            .as_ref()
            .expect("task proxy")
            .lock()
            .task
            .status,
        dsh_types::agent::TaskStatus::Running,
        "a refusal must not stop the task for a person"
    );
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

#[test]
fn create_confirmation_names_the_goal_for_an_agent_job() {
    let request = CronToolRequest {
        action: Some(CronToolAction::Create),
        schedule: Some("5m".to_string()),
        agent: true,
        goal: Some("clean up temp files".to_string()),
        ..CronToolRequest::default()
    };
    let message = confirm_message(CronToolAction::Create, &request);
    assert!(message.contains("clean up temp files"), "{message}");
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
fn update_confirmation_names_a_rename_and_a_raised_token_ceiling() {
    let request = CronToolRequest {
        action: Some(CronToolAction::Update),
        job: Some("foo".to_string()),
        name: Some("bar".to_string()),
        max_tokens_per_day: Some("200000".to_string()),
        check: vec!["tests pass".to_string()],
        ..CronToolRequest::default()
    };
    let message = confirm_message(CronToolAction::Update, &request);
    assert!(message.contains("name=bar"), "{message}");
    assert!(message.contains("max_tokens_per_day=200000"), "{message}");
    assert!(message.contains("tests pass"), "{message}");
}
