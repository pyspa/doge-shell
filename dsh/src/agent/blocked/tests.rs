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

/// A hint truncated mid-entry (see `AgentRuntime::note_denial`'s length
/// cap) must not suggest the truncated text as a grant key: without its
/// closing paren it resolves to no fix, and the full key stays one
/// `agent show` away.
#[test]
fn a_truncated_mcp_hint_offers_no_flag() {
    let mut task = base_task();
    task.status = TaskStatus::Interrupted;
    task.stop_reason = Some(
        "AI wants to call MCP tool: `runner.bash` (external operation needs an exact task grant: mcp:bash:{\"cmd\":\"ls --very-long"
            .into(),
    );
    assert!(blocked_need(&task).is_none());
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

/// A skill script is refused before grants are consulted, so no resume flag
/// can satisfy it: report the reason with no fix, never an `--allow-command`
/// that would fail the instant it runs.
#[test]
fn a_skill_script_refusal_offers_no_flag() {
    for status in [TaskStatus::InputRequired, TaskStatus::Interrupted] {
        let mut task = base_task();
        task.status = status;
        task.stop_reason = Some(
            "bash .dogesh/skills/deploy/scripts/run.sh: running a skill script needs its own approval"
                .into(),
        );
        let need = blocked_need(&task).unwrap();
        assert!(need.fix.is_none(), "{status:?} must not suggest a flag");
        assert!(need.what.contains("skill script"));
    }
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

fn intent(sequence: u64, id: &str, name: &str, args: &str) -> dsh_types::agent::TaskEvent {
    dsh_types::agent::TaskEvent {
        sequence,
        kind: "tool_intent".into(),
        data: serde_json::json!({"id": id, "function": {"name": name, "arguments": args}}),
    }
}

fn tool_result(
    sequence: u64,
    id: &str,
    name: &str,
    args: &str,
    result: &str,
    failed: bool,
) -> dsh_types::agent::TaskEvent {
    dsh_types::agent::TaskEvent {
        sequence,
        kind: "tool_result".into(),
        data: serde_json::json!({"call": {"id": id, "function": {"name": name, "arguments": args}}, "result": result, "failed": failed}),
    }
}

/// A file-write refusal rebuilds to the canonical marker shape, which
/// resolves to the parent-directory `--write` fix.
#[test]
fn events_recover_a_file_write_refusal() {
    let events = vec![
        intent(1, "call-1", "edit", "{\"path\":\"/tmp/proj/src/x.rs\"}"),
        tool_result(
            2,
            "call-1",
            "edit",
            "{\"path\":\"/tmp/proj/src/x.rs\"}",
            "Error: agent: permission required: AI wants to write `/tmp/proj/src/x.rs` [approval_key: write:/tmp/proj/src/x.rs]\nPlease analyze the error and retry with corrected arguments.",
            true,
        ),
    ];
    assert_eq!(
        denial_hint_from_events(&events).as_deref(),
        Some("AI wants to write `/tmp/proj/src/x.rs` [approval_key: write:/tmp/proj/src/x.rs]")
    );
}

/// An exact-command refusal rebuilds from its intent, so the fix names the
/// byte-exact command line even though the result omits the policy reason.
#[test]
fn events_recover_an_exact_command_refusal() {
    let args = "{\"command\":\"cargo test -p foo\"}";
    let events = vec![
        intent(1, "call-1", "execute", args),
        tool_result(
            2,
            "call-1",
            "execute",
            args,
            "Error: agent: command permission required: cargo test -p foo\nPlease analyze the error and retry with corrected arguments.",
            true,
        ),
    ];
    assert_eq!(
        denial_hint_from_events(&events).as_deref(),
        Some("cargo test -p foo: command is not in the task's exact command grants")
    );
}

/// Without a matching intent the command falls back to the result's own
/// first line rather than resolving to nothing.
#[test]
fn events_recover_a_command_refusal_without_its_intent() {
    let events = vec![tool_result(
        1,
        "call-1",
        "execute",
        "{}",
        "Error: agent: command permission required: cargo test -p foo\nPlease analyze the error and retry with corrected arguments.",
        true,
    )];
    assert_eq!(
        denial_hint_from_events(&events).as_deref(),
        Some("cargo test -p foo: command is not in the task's exact command grants")
    );
}

/// A skill-script refusal rebuilds to the ungrantable shape: reported, never
/// offered as a flag.
#[test]
fn events_recover_a_skill_script_refusal_without_a_fix() {
    let args = "{\"command\":\"bash .dogesh/skills/deploy/scripts/run.sh\"}";
    let events = vec![
        intent(1, "call-1", "execute", args),
        tool_result(
            2,
            "call-1",
            "execute",
            args,
            "Error: agent: skill script permission required: bash .dogesh/skills/deploy/scripts/run.sh\nPlease analyze the error and retry with corrected arguments.",
            true,
        ),
    ];
    let hint = denial_hint_from_events(&events).unwrap();
    let mut task = base_task();
    task.status = TaskStatus::Interrupted;
    task.stop_reason = Some(hint);
    let need = blocked_need(&task).unwrap();
    assert!(need.fix.is_none());
}

/// An MCP cancellation rebuilds the canonical message from its intent's
/// exact entry, so the fix byte-matches what `--allow-mcp` expects.
#[test]
fn events_recover_an_mcp_refusal() {
    let args = "{\"cmd\":\"ls\"}";
    let events = vec![
        intent(1, "call-1", "runner.bash", args),
        tool_result(
            2,
            "call-1",
            "runner.bash",
            args,
            "MCP tool execution cancelled by user.",
            true,
        ),
    ];
    assert_eq!(
        denial_hint_from_events(&events).as_deref(),
        Some("AI wants to call MCP tool: `runner.bash` (external operation needs an exact task grant: mcp:runner.bash:{\"cmd\":\"ls\"})")
    );
}

/// Only the newest result counts: a genuine failure after an older refusal
/// must not resurrect the stale grant.
#[test]
fn events_ignore_a_denial_older_than_a_genuine_result() {
    let events = vec![
        intent(1, "call-1", "execute", "{\"command\":\"cargo test\"}"),
        tool_result(
            2,
            "call-1",
            "execute",
            "{\"command\":\"cargo test\"}",
            "Error: agent: command permission required: cargo test\nPlease analyze the error and retry with corrected arguments.",
            true,
        ),
        intent(3, "call-2", "execute", "{\"command\":\"cargo test\"}"),
        tool_result(4, "call-2", "execute", "{\"command\":\"cargo test\"}", "test failed", true),
    ];
    assert!(denial_hint_from_events(&events).is_none());
}

#[test]
fn events_with_no_failed_tool_result_recover_nothing() {
    assert!(denial_hint_from_events(&[]).is_none());
    let events = vec![
        intent(1, "call-1", "read_file", "{}"),
        tool_result(2, "call-1", "read_file", "{}", "file contents", false),
    ];
    assert!(denial_hint_from_events(&events).is_none());
}

/// Since deny-and-continue, a task that works around refusals until it
/// cannot proceed lands `Interrupted` with the refusal hint - it must offer
/// the same resume fix an `InputRequired` task would.
#[test]
fn an_interrupted_task_with_a_grant_hint_offers_the_fix() {
    let mut task = base_task();
    task.status = TaskStatus::Interrupted;
    task.stop_reason =
        Some("cargo test -p foo: command is not in the task's exact command grants".into());
    let need = blocked_need(&task).unwrap();
    assert_eq!(
        need.fix.unwrap(),
        "agent resume task-1 --allow-command 'cargo test -p foo'"
    );
}

#[test]
fn an_interrupted_task_without_a_grant_hint_has_no_need() {
    for reason in [
        "task stopped before completion (budget or interruption)",
        "verification remains incomplete",
        super::super::store::RECOVERED_STOP_REASON,
    ] {
        let mut task = base_task();
        task.status = TaskStatus::Interrupted;
        task.stop_reason = Some(reason.into());
        assert!(blocked_need(&task).is_none(), "{reason}");
    }
}

#[test]
fn a_running_task_has_no_need() {
    let mut task = base_task();
    task.status = TaskStatus::Running;
    assert!(blocked_need(&task).is_none());
}
