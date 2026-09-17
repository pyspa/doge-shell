use super::*;
use dsh_types::agent::TaskGrant;

fn minimal_task(root: &std::path::Path, status: TaskStatus) -> AgentTask {
    AgentTask {
        id: uuid::Uuid::new_v4().to_string(),
        goal: "line one\nline two".into(),
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

#[test]
fn age_is_formatted_in_the_coarsest_useful_unit() {
    assert_eq!(format_age(100, 100), "0s");
    assert_eq!(format_age(160, 100), "1m");
    assert_eq!(format_age(100 + 3 * 3600, 100), "3h");
    assert_eq!(format_age(100 + 2 * 86400, 100), "2d");
}

#[test]
fn goal_preview_flattens_newlines_and_clamps() {
    assert_eq!(goal_preview("line one\nline two"), "line one line two");
    let long = "x".repeat(200);
    // `clamp_chars` cuts at 60 chars and appends "...", so the result is 63
    // chars, not exactly 60.
    assert_eq!(goal_preview(&long), format!("{}...", "x".repeat(60)));
}

#[test]
fn still_going_is_true_while_the_task_holds_its_lock_even_before_status_flips() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    // A detached child can hold the lock before `run_task` ever sets
    // `Running` - `still_going` must not treat that window as finished.
    let task = minimal_task(dir.path(), TaskStatus::Interrupted);
    let lock = locks::try_lock_task(&store, &task.id).unwrap().unwrap();
    assert!(still_going(&store, &task));
    drop(lock);
    assert!(!still_going(&store, &task));
}

#[test]
fn still_going_is_true_for_a_running_status_even_without_a_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let task = minimal_task(dir.path(), TaskStatus::Running);
    assert!(still_going(&store, &task));
}

#[test]
fn still_going_is_false_for_a_settled_task_with_no_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    for status in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
        TaskStatus::InputRequired,
        TaskStatus::Interrupted,
    ] {
        let task = minimal_task(dir.path(), status);
        assert!(!still_going(&store, &task), "{status:?}");
    }
}

#[test]
fn list_options_reject_unknown_flags_but_accept_all_and_json() {
    assert!(parse_list_options(&[]).is_ok());
    let opts = parse_list_options(&["--all".to_string(), "--json".to_string()]).unwrap();
    assert!(opts.all && opts.json);
    assert!(parse_list_options(&["--bogus".to_string()]).is_err());
}

#[test]
fn logs_options_require_an_id_and_parse_since() {
    assert!(parse_logs_options(&[]).is_err());
    let args = vec![
        "task-1".to_string(),
        "--follow".to_string(),
        "--since".to_string(),
        "42".to_string(),
    ];
    let (id, options) = parse_logs_options(&args).unwrap();
    assert_eq!(id, "task-1");
    assert!(options.follow);
    assert_eq!(options.since, 42);
}

#[test]
fn wait_options_require_an_id_and_parse_timeout() {
    assert!(parse_wait_options(&[]).is_err());
    let args = vec![
        "task-1".to_string(),
        "--timeout".to_string(),
        "30".to_string(),
    ];
    let (id, options) = parse_wait_options(&args).unwrap();
    assert_eq!(id, "task-1");
    assert_eq!(options.timeout_secs, Some(30));
}

#[test]
fn render_event_line_names_the_tool_for_intents_and_results() {
    let intent = TaskEvent {
        sequence: 1,
        kind: "tool_intent".to_string(),
        data: json!({"function": {"name": "execute", "arguments": "{\"command\":\"ls\"}"}}),
    };
    assert!(render_event_line(&intent).contains("execute"));

    let result = TaskEvent {
        sequence: 2,
        kind: "tool_result".to_string(),
        data: json!({
            "call": {"function": {"name": "read_file"}},
            "result": "contents",
            "failed": false,
        }),
    };
    let line = render_event_line(&result);
    assert!(line.contains("read_file"));
    assert!(line.contains("ok"));
}
