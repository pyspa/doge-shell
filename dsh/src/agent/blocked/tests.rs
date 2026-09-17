use super::*;
use dsh_types::agent::{TaskGrant, TaskStatus};

fn base_task() -> AgentTask {
    AgentTask {
        id: "task-1".to_string(),
        goal: "do something".into(),
        root: "/tmp".into(),
        status: TaskStatus::InputRequired,
        grant: TaskGrant::default(),
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        token_budget: 1000,
        tokens_used: 0,
        time_budget_ms: 1000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}

#[test]
fn pending_operation_wins_over_everything_else() {
    let mut task = base_task();
    task.pending_operation = Some(serde_json::json!({"anything": true}));
    task.stop_reason = Some("AI wants to write `x` [approval_key: write:/tmp/x]".into());
    let need = blocked_need(&task).unwrap();
    assert!(need.fix.unwrap().contains("--reconcile"));
}

#[test]
fn write_key_suggests_the_parent_directory() {
    let mut task = base_task();
    task.stop_reason = Some(
        "AI wants to write `/tmp/proj/src/x.rs` [approval_key: write:/tmp/proj/src/x.rs]".into(),
    );
    let need = blocked_need(&task).unwrap();
    assert_eq!(
        need.fix.unwrap(),
        "agent resume task-1 --write /tmp/proj/src"
    );
}

#[test]
fn mcp_key_suggests_allow_mcp_verbatim() {
    // Defensive: nothing in production actually writes an `[approval_key:
    // mcp:...]` bracket today (see `real_mcp_stop_reason_also_suggests_allow_mcp`
    // for the shape that really occurs) - kept in case `from_approval_key`'s
    // `mcp:` branch ever does become reachable.
    let mut task = base_task();
    task.stop_reason =
        Some("AI wants to call runner.bash [approval_key: mcp:bash:{\"cmd\":\"ls\"}]".into());
    let need = blocked_need(&task).unwrap();
    assert_eq!(
        need.fix.unwrap(),
        "agent resume task-1 --allow-mcp 'mcp:bash:{\"cmd\":\"ls\"}'"
    );
}

#[test]
fn real_mcp_stop_reason_also_suggests_allow_mcp() {
    // The shape `authorize_mcp_tool` (`dsh-builtin/src/chatgpt/tool/mod.rs`)
    // and `evaluate_agent_tool` (`dsh/src/proxy/agent_policy.rs`) actually
    // produce: no `[approval_key: ...]` marker at all, since MCP calls never
    // go through `confirm_agent_action_with_preview`.
    let mut task = base_task();
    task.stop_reason = Some(
        "AI wants to call MCP tool: `runner.bash` (external operation needs an exact task grant: mcp:bash:{\"cmd\":\"ls\"})"
            .into(),
    );
    let need = blocked_need(&task).unwrap();
    assert_eq!(
        need.fix.unwrap(),
        "agent resume task-1 --allow-mcp 'mcp:bash:{\"cmd\":\"ls\"}'"
    );
}

#[test]
fn cron_pause_resume_remove_run_suggest_a_verbatim_command() {
    for action in ["pause", "resume", "remove", "run"] {
        let mut task = base_task();
        task.stop_reason = Some(format!(
            "AI wants to change cron job `digest` [approval_key: cron:{action}:digest]"
        ));
        let need = blocked_need(&task).unwrap();
        assert_eq!(
            need.fix.unwrap(),
            format!("cron {action} digest && agent resume task-1"),
            "{action}"
        );
    }
}

#[test]
fn cron_create_update_and_ack_offer_no_flag() {
    // None of these map onto a bare `cron <action> <job>`: `create` needs a
    // schedule and command/`--agent` this key does not carry, there is no
    // `cron update` subcommand (the real verb is `cron edit`), and `ack` is
    // `cron incidents ack <id>`, not `cron ack <id>` - offering any of them
    // verbatim would just fail when the person ran it.
    for key in ["cron:create:digest", "cron:update:digest", "cron:ack:?"] {
        let mut task = base_task();
        task.stop_reason = Some(format!(
            "AI wants to change cron job `digest` [approval_key: {key}]"
        ));
        let need = blocked_need(&task).unwrap();
        assert!(need.fix.is_none(), "{key} must not suggest a flag");
    }
}

#[test]
fn hook_and_sensitive_keys_offer_no_flag() {
    for key in [
        "hook:my-hook:execute",
        "sensitive:read:/etc/passwd",
        "delete:/tmp/skill.md",
    ] {
        let mut task = base_task();
        task.stop_reason = Some(format!("blocked [approval_key: {key}]"));
        let need = blocked_need(&task).unwrap();
        assert!(need.fix.is_none(), "{key} must not suggest a flag");
    }
}

#[test]
fn a_plain_command_confirm_extracts_the_exact_command_line() {
    let mut task = base_task();
    task.stop_reason =
        Some("cargo test -p foo: command is not in the task's exact command grants".into());
    let need = blocked_need(&task).unwrap();
    assert_eq!(
        need.fix.unwrap(),
        "agent resume task-1 --allow-command 'cargo test -p foo'"
    );
}

#[test]
fn unknown_shapes_offer_no_flag_but_still_report_the_reason() {
    let mut task = base_task();
    task.stop_reason = Some("budget exhausted before verification".into());
    let need = blocked_need(&task).unwrap();
    assert!(need.fix.is_none());
    assert_eq!(need.what, "budget exhausted before verification");
}

#[test]
fn a_task_that_is_not_input_required_has_no_need() {
    let mut task = base_task();
    task.status = TaskStatus::Completed;
    task.stop_reason = Some("[approval_key: write:/tmp/x]".into());
    assert!(blocked_need(&task).is_none());
}

#[test]
fn a_running_task_has_no_need() {
    let mut task = base_task();
    task.status = TaskStatus::Running;
    assert!(blocked_need(&task).is_none());
}
