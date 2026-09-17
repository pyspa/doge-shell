use super::*;
use dsh_types::agent::TaskGrant;

fn task(id: &str, status: TaskStatus) -> AgentTask {
    AgentTask {
        id: id.to_string(),
        goal: "do something".into(),
        root: "/tmp".into(),
        status,
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
fn the_first_scan_never_notifies() {
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let tasks = vec![task("a", TaskStatus::Completed)];
    let lines = notices_for(&mut seen, &tasks, &detached, true);
    assert!(lines.is_empty());
    // But it must still have recorded the status, so the *next* scan can
    // compare against it.
    assert_eq!(seen.get("a"), Some(&TaskStatus::Completed));
}

#[test]
fn a_transition_away_from_running_notifies_once() {
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    seen.insert("a".to_string(), TaskStatus::Running);

    let tasks = vec![task("a", TaskStatus::Completed)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert_eq!(lines.len(), 1);

    // Scanning the same (unchanged) status again must not notify twice.
    let lines_again = notices_for(&mut seen, &tasks, &detached, false);
    assert!(lines_again.is_empty());
}

#[test]
fn a_task_not_in_the_detached_set_is_never_notified() {
    let mut seen = HashMap::new();
    seen.insert("a".to_string(), TaskStatus::Running);
    let detached: HashSet<String> = HashSet::new(); // foreground `agent run`
    let tasks = vec![task("a", TaskStatus::Completed)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert!(lines.is_empty());
}

#[test]
fn input_required_is_also_a_notifiable_transition() {
    let mut seen = HashMap::new();
    seen.insert("a".to_string(), TaskStatus::Running);
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let tasks = vec![task("a", TaskStatus::InputRequired)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert_eq!(lines.len(), 1);
}

#[test]
fn a_task_that_finished_between_two_scans_still_notifies_once() {
    // A short enough `--detach` run can start and finish entirely inside
    // one idle poll gap, so this session never observes it `Running` at
    // all - there is no transition *from* anything, but its first-ever
    // observation already being settled is itself the news.
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let tasks = vec![task("a", TaskStatus::Completed)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert_eq!(lines.len(), 1);

    // Scanning the same (unchanged) status again must not notify twice.
    let lines_again = notices_for(&mut seen, &tasks, &detached, false);
    assert!(lines_again.is_empty());
}

#[test]
fn a_task_first_observed_as_interrupted_does_not_notify() {
    // `detach::start` saves a task as `Interrupted` before its child ever
    // marks it `Running` - the first scan to see a freshly detached task
    // can land in that window, and it must not be mistaken for a task that
    // failed to start.
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let tasks = vec![task("a", TaskStatus::Interrupted)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert!(lines.is_empty());
}

/// A grant-stuck run that starts and finishes between two scans must still
/// notify: unlike the pre-start transient above it carries its refusal hint
/// in `stop_reason`, which is also what renders the resume command.
#[test]
fn a_task_first_observed_as_grant_stuck_interrupted_notifies() {
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let mut stuck = task("a", TaskStatus::Interrupted);
    stuck.stop_reason =
        Some("cargo test: command is not in the task's exact command grants".into());
    let lines = notices_for(&mut seen, &[stuck], &detached, false);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("--allow-command 'cargo test'"));
}

#[test]
fn a_task_first_observed_as_running_does_not_notify() {
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let tasks = vec![task("a", TaskStatus::Running)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert!(lines.is_empty());
}

/// The exact regression this exists to catch: a task created (and first
/// scanned) as a plain, non-detached `agent run`, later `agent resume`d
/// with `--detach`. Gating the check on `seen` instead of `detached` would
/// have skipped it forever once the first scan had recorded *any* status
/// for it.
#[test]
fn a_task_detached_after_its_first_scan_is_still_caught() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::agent::SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut running = task("a", TaskStatus::Running);
    running.root = dir.path().canonicalize().unwrap();
    store.save(&running, None).unwrap();

    let mut detached = HashSet::new();
    refresh_detached_set(&store, &[running.clone()], &mut detached);
    assert!(
        detached.is_empty(),
        "not yet detached; nothing should be recorded"
    );

    store
        .save(&running, Some(("detached", &serde_json::json!({"pid": 1}))))
        .unwrap();
    refresh_detached_set(&store, &[running.clone()], &mut detached);
    assert!(detached.contains("a"), "the later --detach must be noticed");
}

#[test]
fn a_task_already_confirmed_detached_is_not_rechecked() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::agent::SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let t = task("a", TaskStatus::Running);
    let mut detached: HashSet<String> = ["a".to_string()].into_iter().collect();

    // No "detached" event exists at all, and no such task is even saved in
    // the store - if this looked it up, `store.events` would still return
    // an empty (not erroring) list, so this only proves the short-circuit
    // skips the lookup rather than merely tolerating its absence.
    refresh_detached_set(&store, &[t], &mut detached);
    assert!(detached.contains("a"));
}

#[test]
fn watch_defaults_to_enabled() {
    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    assert!(resolve_enabled(&mut shell));
}

#[test]
fn watch_can_be_turned_off() {
    use dsh_builtin::ShellProxy;
    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    shell.set_var("DOGESH_AGENT_WATCH".into(), "off".into());
    assert!(!resolve_enabled(&mut shell));
}

#[test]
fn the_interval_is_clamped_to_a_sane_range() {
    use dsh_builtin::ShellProxy;
    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    shell.set_var("DOGESH_AGENT_WATCH_INTERVAL_SECS".into(), "9999".into());
    assert_eq!(resolve_active_interval_secs(&mut shell), 60);
    shell.set_var("DOGESH_AGENT_WATCH_INTERVAL_SECS".into(), "0".into());
    assert_eq!(resolve_active_interval_secs(&mut shell), 1);
}
