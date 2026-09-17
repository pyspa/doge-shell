use super::*;
use dsh_types::agent::TaskGrant;

fn minimal_task(root: &std::path::Path, status: TaskStatus) -> AgentTask {
    AgentTask {
        id: uuid::Uuid::new_v4().to_string(),
        goal: "do something".into(),
        root: root.canonicalize().unwrap(),
        status,
        grant: TaskGrant {
            read_roots: vec![root.canonicalize().unwrap()],
            ..Default::default()
        },
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        tokens_used: 0,
        time_budget_ms: 60_000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}

#[test]
fn detach_deadline_adds_a_flat_grace_period_past_the_time_budget() {
    assert_eq!(detach_deadline_secs(0), DETACH_WATCHDOG_GRACE_SECS);
    assert_eq!(
        detach_deadline_secs(120_000),
        120 + DETACH_WATCHDOG_GRACE_SECS
    );
}

#[test]
fn a_completed_or_cancelled_task_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for status in [TaskStatus::Completed, TaskStatus::Cancelled] {
        let task = minimal_task(dir.path(), status);
        assert!(refuse_if_finished(&task).is_err(), "{status:?}");
    }
}

#[test]
fn a_task_with_work_left_is_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    for status in [
        TaskStatus::Running,
        TaskStatus::Interrupted,
        TaskStatus::InputRequired,
        TaskStatus::Failed,
    ] {
        let task = minimal_task(dir.path(), status);
        assert!(refuse_if_finished(&task).is_ok(), "{status:?}");
    }
}

#[test]
fn open_private_log_creates_a_private_appendable_file_and_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("run.log");
    let mut file = open_private_log(&path).unwrap();
    use std::io::Write;
    writeln!(file, "first").unwrap();
    drop(file);
    let mut file = open_private_log(&path).unwrap();
    writeln!(file, "second").unwrap();
    drop(file);
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "first\nsecond\n");
    let mode = std::fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(mode.mode() & 0o777, 0o600);
}
