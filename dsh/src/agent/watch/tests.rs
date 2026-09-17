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
fn a_task_that_was_never_seen_running_does_not_notify() {
    // e.g. a task that finished between two scans so fast this session never
    // observed it as `Running` - nothing to announce a transition *from*.
    let mut seen = HashMap::new();
    let detached: HashSet<String> = ["a".to_string()].into_iter().collect();
    let tasks = vec![task("a", TaskStatus::Completed)];
    let lines = notices_for(&mut seen, &tasks, &detached, false);
    assert!(lines.is_empty());
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
