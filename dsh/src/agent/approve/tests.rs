use super::*;
use dsh_types::agent::TaskGrant;

fn task_with(status: TaskStatus, stop_reason: Option<&str>) -> AgentTask {
    AgentTask {
        id: "task-1".to_string(),
        goal: "do something".into(),
        root: "/tmp".into(),
        status,
        grant: TaskGrant::default(),
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        tokens_used: 0,
        time_budget_ms: 60_000,
        elapsed_ms: 0,
        stop_reason: stop_reason.map(str::to_string),
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}

fn grantable(resolution: ApprovalResolution) -> GrantApproval {
    match resolution {
        ApprovalResolution::Grantable(approval) => approval,
        other => panic!("expected grantable, got {}", describe(&other)),
    }
}

fn describe(resolution: &ApprovalResolution) -> &'static str {
    match resolution {
        ApprovalResolution::Grantable(_) => "grantable",
        ApprovalResolution::Ungrantable { .. } => "ungrantable",
        ApprovalResolution::NothingFound { .. } => "nothing-found",
    }
}

#[test]
fn a_write_hint_resolves_to_its_parent_directory() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("AI wants to write `/tmp/proj/src/x.rs` [approval_key: write:/tmp/proj/src/x.rs]"),
    );
    let approval = grantable(resolve_approval(&task, &[]));
    assert_eq!(approval.option, "--write");
    assert_eq!(approval.value, "/tmp/proj/src");
}

#[test]
fn a_command_hint_resolves_to_the_exact_command() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("cargo test -p foo: command is not in the task's exact command grants"),
    );
    let approval = grantable(resolve_approval(&task, &[]));
    assert_eq!(approval.option, "--allow-command");
    assert_eq!(approval.value, "cargo test -p foo");
}

#[test]
fn an_mcp_hint_resolves_to_the_exact_entry() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("AI wants to call MCP tool: `runner.bash` (external operation needs an exact task grant: mcp:bash:{\"cmd\":\"ls\"})"),
    );
    let approval = grantable(resolve_approval(&task, &[]));
    assert_eq!(approval.option, "--allow-mcp");
    assert_eq!(approval.value, "mcp:bash:{\"cmd\":\"ls\"}");
}

#[test]
fn a_skill_script_hint_is_reported_never_offered() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("bash run.sh: running a skill script needs its own approval"),
    );
    match resolve_approval(&task, &[]) {
        ApprovalResolution::Ungrantable { what, guidance } => {
            assert!(what.contains("skill script"));
            assert!(guidance.contains("--reconcile"));
        }
        other => panic!("expected ungrantable, got {}", describe(&other)),
    }
}

#[test]
fn a_hook_key_is_reported_never_offered() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("hook `h` flagged `execute` [approval_key: hook:h:execute]"),
    );
    assert_eq!(
        describe(&resolve_approval(&task, &[])),
        "ungrantable"
    );
}

#[test]
fn a_task_with_no_grant_refusal_resolves_to_nothing() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("task stopped before completion (budget or interruption)"),
    );
    assert_eq!(describe(&resolve_approval(&task, &[])), "nothing-found");
    let done = task_with(TaskStatus::Completed, None);
    assert_eq!(describe(&resolve_approval(&done, &[])), "nothing-found");
}

#[test]
fn a_recorded_hint_wins_over_an_older_events_refusal() {
    let task = task_with(
        TaskStatus::Interrupted,
        Some("cargo test: command is not in the task's exact command grants"),
    );
    let events = vec![dsh_types::agent::TaskEvent {
        sequence: 1,
        kind: "tool_result".into(),
        data: serde_json::json!({"call": {"id": "c", "function": {"name": "edit", "arguments": "{}"}}, "result": "Error: agent: permission required: AI wants to write `/other/x` [approval_key: write:/other/x]\nPlease analyze.", "failed": true}),
    }];
    let approval = grantable(resolve_approval(&task, &events));
    assert_eq!(approval.value, "cargo test");
}

#[test]
fn events_refusal_resolves_when_the_reason_carries_no_hint() {
    let task = task_with(
        TaskStatus::InputRequired,
        Some("same operation failed three times; inspect the cause before resuming"),
    );
    let events = vec![dsh_types::agent::TaskEvent {
        sequence: 1,
        kind: "tool_result".into(),
        data: serde_json::json!({"call": {"id": "c", "function": {"name": "execute", "arguments": "{\"command\":\"cargo test\"}"}}, "result": "Error: agent: command permission required: cargo test\nPlease analyze.", "failed": true}),
    }];
    let approval = grantable(resolve_approval(&task, &events));
    assert_eq!(approval.option, "--allow-command");
    assert_eq!(approval.value, "cargo test");
}

fn shell() -> crate::shell::Shell {
    crate::shell::Shell::new(crate::environment::Environment::new())
}

fn stored_task(dir: &std::path::Path, status: TaskStatus, stop_reason: Option<&str>) -> (SqliteTaskStore, String) {
    let store = SqliteTaskStore::open(&dir.join("state")).unwrap();
    let mut task = task_with(status, stop_reason);
    task.id = uuid::Uuid::new_v4().to_string();
    task.root = dir.canonicalize().unwrap();
    store.save(&task, None).unwrap();
    let id = task.id.clone();
    (store, id)
}

fn ctx_for(shell: &crate::shell::Shell) -> Context {
    Context::new_safe(shell.pid, shell.pgid, false)
}

#[test]
fn approve_refuses_a_running_task_before_any_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, id) = stored_task(dir.path(), TaskStatus::Running, None);
    let store = Arc::new(store);
    let mut shell = shell();
    let ctx = ctx_for(&shell);
    let error = approve(&mut shell, &ctx, &store, &[id]).unwrap_err();
    assert!(error.to_string().contains("already running"), "{error:#}");
}

#[test]
fn approve_refuses_a_finished_task() {
    let dir = tempfile::tempdir().unwrap();
    let (store, id) = stored_task(dir.path(), TaskStatus::Completed, None);
    let store = Arc::new(store);
    let mut shell = shell();
    let ctx = ctx_for(&shell);
    let error = approve(&mut shell, &ctx, &store, &[id]).unwrap_err();
    assert!(error.to_string().contains("already finished"), "{error:#}");
}

#[test]
fn approve_requires_reconcile_for_a_pending_operation() {
    let dir = tempfile::tempdir().unwrap();
    let (store, id) = stored_task(
        dir.path(),
        TaskStatus::Interrupted,
        Some("cargo test: command is not in the task's exact command grants"),
    );
    let store = Arc::new(store);
    {
        let mut task = store.load(&id).unwrap();
        task.pending_operation = Some(serde_json::json!({"id": "lost"}));
        store.save(&task, None).unwrap();
    }
    let mut shell = shell();
    let ctx = ctx_for(&shell);
    let error = approve(&mut shell, &ctx, &store, &[id]).unwrap_err();
    assert!(error.to_string().contains("--reconcile"), "{error:#}");
}

#[test]
fn approve_dry_run_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, id) = stored_task(
        dir.path(),
        TaskStatus::Interrupted,
        Some("cargo test: command is not in the task's exact command grants"),
    );
    let store = Arc::new(store);
    let mut shell = shell();
    let ctx = ctx_for(&shell);
    approve(&mut shell, &ctx, &store, &[id.clone(), "--dry-run".into()]).unwrap();
    assert!(store.load(&id).unwrap().grant.commands.is_empty());
}

#[test]
fn approve_is_a_no_op_when_the_grant_is_already_there() {
    let dir = tempfile::tempdir().unwrap();
    let (store, id) = stored_task(
        dir.path(),
        TaskStatus::Interrupted,
        Some("cargo test: command is not in the task's exact command grants"),
    );
    let store = Arc::new(store);
    {
        let mut task = store.load(&id).unwrap();
        task.grant.commands.push("cargo test".to_string());
        store.save(&task, None).unwrap();
    }
    let mut shell = shell();
    let ctx = ctx_for(&shell);
    approve(&mut shell, &ctx, &store, std::slice::from_ref(&id)).unwrap();
    assert_eq!(store.load(&id).unwrap().grant.commands.len(), 1);
}

#[test]
fn approve_rejects_unknown_options() {
    let dir = tempfile::tempdir().unwrap();
    let (store, id) = stored_task(dir.path(), TaskStatus::Interrupted, None);
    let store = Arc::new(store);
    let mut shell = shell();
    let ctx = ctx_for(&shell);
    let error = approve(&mut shell, &ctx, &store, &[id, "--detach".into()]).unwrap_err();
    assert!(error.to_string().contains("unsupported option"), "{error:#}");
}
