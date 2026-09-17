use super::*;
use dsh_types::agent::{AgentTask, TaskGrant};

fn minimal_task(root: &std::path::Path, status: TaskStatus) -> AgentTask {
    AgentTask {
        id: uuid::Uuid::new_v4().to_string(),
        goal: "do something".into(),
        root: root.canonicalize().unwrap(),
        status,
        grant: TaskGrant::default(),
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        token_budget: 1000,
        tokens_used: 0,
        time_budget_ms: 60_000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}

fn shell() -> crate::shell::Shell {
    crate::shell::Shell::new(crate::environment::Environment::new())
}

#[test]
fn an_empty_store_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.starts_with("ok no tasks recorded"))
    );
    // Budgets now fall back to built-in defaults, so this report no longer
    // depends on ambient `AI_AGENT_*` process state. What matters for "clean"
    // is that nothing here is about an actual task going wrong.
    assert!(!report.lines.iter().any(|line| {
        line.contains("crashed") || line.contains("needs-") || line.contains("stale-artifacts")
    }));
}

#[test]
fn a_task_recovered_from_a_dead_process_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut task = minimal_task(dir.path(), TaskStatus::Running);
    store.save(&task, None).unwrap();
    // Nobody holds this task's lock, so `recover_interrupted` marks it
    // `Interrupted` and leaves a `recovered` event as the tell-tale.
    store.recover_interrupted().unwrap();
    task = store.load(&task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Interrupted);

    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.starts_with("warn crashed"))
    );
}

#[test]
fn a_fresh_input_required_task_is_not_yet_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut task = minimal_task(dir.path(), TaskStatus::InputRequired);
    task.stop_reason = Some("blocked [approval_key: write:/tmp/x]".into());
    task.created_at = chrono::Utc::now().timestamp(); // just now
    store.save(&task, None).unwrap();

    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    assert!(
        !report
            .lines
            .iter()
            .any(|line| line.contains("needs-approval"))
    );
}

#[test]
fn a_long_stale_input_required_task_is_a_warning_with_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut task = minimal_task(dir.path(), TaskStatus::InputRequired);
    task.stop_reason = Some("blocked [approval_key: write:/tmp/proj/x]".into());
    task.created_at = 0; // ancient
    store.save(&task, None).unwrap();

    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    let line = report
        .lines
        .iter()
        .find(|line| line.contains("needs-approval"))
        .expect("expected a needs-approval warning");
    assert!(line.contains("--write /tmp/proj"));
}

#[test]
fn a_stale_grant_stuck_interrupted_task_is_a_needs_grant_warning() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut task = minimal_task(dir.path(), TaskStatus::Interrupted);
    task.stop_reason = Some("blocked [approval_key: write:/tmp/proj/x]".into());
    task.created_at = 0; // ancient
    store.save(&task, None).unwrap();

    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    let line = report
        .lines
        .iter()
        .find(|line| line.contains("needs-grant"))
        .expect("expected a needs-grant warning");
    assert!(line.contains("--write /tmp/proj"));
}

#[test]
fn a_pending_operation_is_a_reconcile_warning_regardless_of_age() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut task = minimal_task(dir.path(), TaskStatus::InputRequired);
    task.pending_operation = Some(serde_json::json!({"id": "lost"}));
    task.created_at = chrono::Utc::now().timestamp();
    store.save(&task, None).unwrap();

    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.contains("needs-reconcile"))
    );
}

#[test]
fn a_stale_artifact_directory_is_counted_but_not_removed() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let orphan = uuid::Uuid::new_v4().to_string();
    let orphan_dir = dir.path().join("state").join(&orphan);
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(orphan_dir.join("run.log"), b"leftover").unwrap();

    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.starts_with("warn stale-artifacts 1"))
    );
    assert!(orphan_dir.exists());
}

#[test]
fn json_output_carries_the_same_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut shell = shell();
    let report = build_report(&mut shell, &store).unwrap();
    let value = report.to_json();
    assert_eq!(value["ok"], serde_json::json!(report.ok));
    assert_eq!(value["warn"], serde_json::json!(report.warn));
}
