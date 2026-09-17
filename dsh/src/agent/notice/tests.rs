use super::*;
use dsh_types::agent::TaskGrant;

fn task(id: &str, status: TaskStatus, goal: &str) -> AgentTask {
    AgentTask {
        id: id.to_string(),
        goal: goal.to_string(),
        root: "/tmp".into(),
        status,
        grant: TaskGrant::default(),
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
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
fn a_completed_task_shows_its_goal() {
    let t = task(
        "1a2b3c4d-0000-0000-0000-000000000000",
        TaskStatus::Completed,
        "fix the failing test",
    );
    let line = render(&t);
    assert!(line.starts_with("[agent 1a2b3c4d]"));
    assert!(line.contains("completed"));
    assert!(line.contains("fix the failing test"));
}

#[test]
fn a_multiline_goal_is_flattened_to_one_line() {
    let t = task("id", TaskStatus::Failed, "line one\nline two");
    let line = render(&t);
    assert!(!line.contains('\n'));
    assert!(line.contains("line one⏎line two"));
}

#[test]
fn input_required_with_a_known_fix_shows_the_fix_not_the_goal() {
    let mut t = task("id", TaskStatus::InputRequired, "do the thing");
    t.stop_reason =
        Some("cargo test -p foo: command is not in the task's exact command grants".into());
    let line = render(&t);
    assert!(line.contains("Needs approval"));
    assert!(line.contains("--allow-command 'cargo test -p foo'"));
    assert!(!line.contains("do the thing"));
}

#[test]
fn input_required_with_no_resolvable_fix_falls_back_to_the_goal() {
    let mut t = task("id", TaskStatus::InputRequired, "do the thing");
    t.stop_reason = Some("budget exhausted".into());
    let line = render(&t);
    assert!(line.contains("input-required"));
    assert!(line.contains("do the thing"));
}

#[test]
fn an_interrupted_task_with_a_grant_hint_shows_the_fix_not_the_goal() {
    let mut t = task("id", TaskStatus::Interrupted, "do the thing");
    t.stop_reason =
        Some("cargo test -p foo: command is not in the task's exact command grants".into());
    let line = render(&t);
    assert!(line.contains("Needs grant"));
    assert!(line.contains("--allow-command 'cargo test -p foo'"));
    assert!(!line.contains("do the thing"));
}

#[test]
fn an_interrupted_task_without_a_grant_hint_falls_back_to_the_goal() {
    let mut t = task("id", TaskStatus::Interrupted, "do the thing");
    t.stop_reason = Some("task stopped before completion (budget or interruption)".into());
    let line = render(&t);
    assert!(line.contains("interrupted"));
    assert!(line.contains("do the thing"));
}

#[test]
fn short_id_never_panics_on_a_short_string() {
    assert_eq!(short_id("abc"), "abc");
}
