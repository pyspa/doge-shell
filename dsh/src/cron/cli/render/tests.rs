use super::*;
use dsh_types::cron::job::{IncidentKind, JobKind, RunReason, RunTrigger};
use dsh_types::schedule::{NotifyPolicy, parse_schedule};

fn job(name: &str) -> CronJobView {
    CronJobView {
        id: 1,
        name: name.to_string(),
        kind: JobKind::Sh,
        schedule: parse_schedule("5m").unwrap(),
        schedule_spec: "5m".to_string(),
        command: "true".to_string(),
        agent: None,
        cwd: "/tmp".to_string(),
        notify: NotifyPolicy::default(),
        timeout_secs: 60,
        catchup_secs: 3600,
        paused: false,
        blocked: false,
        next_run_at: Some(1_000),
        running: false,
        run_count: 0,
        fail_count: 0,
        consecutive_failures: 0,
        last: None,
    }
}

fn run(state: RunState) -> CronRun {
    CronRun {
        id: "r1".to_string(),
        job_id: 1,
        job_name: "probe".to_string(),
        scheduled_for: 900,
        state,
        reason: None,
        started_at: Some(900),
        finished_at: Some(905),
        duration_ms: 5_000,
        exit_code: 0,
        timed_out: false,
        changed: false,
        trigger: RunTrigger::Tick,
        agent_task_id: None,
        tokens_used: 0,
        pending_skills: 0,
        preview: "hello".to_string(),
    }
}

#[test]
fn an_empty_job_list_points_at_how_to_add_one() {
    assert!(render_job_list(&[], 0).contains("cron add"));
}

#[test]
fn a_paused_job_shows_paused_regardless_of_next_run_at() {
    let mut j = job("probe");
    j.paused = true;
    assert_eq!(describe_next(&j, 500), "paused");
}

#[test]
fn a_blocked_job_shows_blocked_even_when_due() {
    let mut j = job("probe");
    j.blocked = true;
    j.next_run_at = Some(0);
    assert_eq!(describe_next(&j, 500), "blocked");
}

#[test]
fn a_due_job_says_due_not_a_negative_duration() {
    let j = job("probe");
    assert_eq!(describe_next(&j, 1_000), "due");
    assert_eq!(describe_next(&j, 2_000), "due");
}

#[test]
fn a_future_job_counts_down() {
    let j = job("probe");
    assert_eq!(describe_next(&j, 700), "in 5m0s");
}

#[test]
fn a_job_with_no_next_run_shows_a_dash() {
    let mut j = job("probe");
    j.next_run_at = None;
    assert_eq!(describe_next(&j, 0), "-");
}

#[test]
fn the_job_table_includes_every_job_by_name() {
    let table = render_job_list(&[job("alpha"), job("beta")], 0);
    assert!(table.contains("alpha"));
    assert!(table.contains("beta"));
}

#[test]
fn last_run_is_a_dash_when_there_is_none() {
    assert_eq!(describe_last(None), "-");
}

#[test]
fn a_successful_last_run_says_ok() {
    assert!(describe_last(Some(&run(RunState::Succeeded))).starts_with("ok"));
}

#[test]
fn a_skipped_run_names_its_reason() {
    let mut r = run(RunState::Skipped);
    r.reason = Some(RunReason::AgentBusy);
    assert!(
        describe_last(Some(&r)).contains("agent-busy"),
        "{}",
        describe_last(Some(&r))
    );
}

#[test]
fn empty_history_says_so_instead_of_an_empty_table() {
    assert_eq!(render_history(&[]), "No runs recorded yet.");
}

#[test]
fn history_includes_the_agent_task_id_when_there_is_one() {
    let mut r = run(RunState::Succeeded);
    r.agent_task_id = Some("task-42".to_string());
    let table = render_history(&[r]);
    assert!(table.contains("task-42"), "{table}");
}

#[test]
fn empty_incidents_says_so() {
    assert_eq!(render_incidents(&[]), "No incidents.");
}

#[test]
fn an_open_incident_is_labelled_open_not_acked() {
    let incident = CronIncident {
        id: 1,
        job_id: Some(1),
        job_name: Some("probe".to_string()),
        opened_at: 0,
        acked_at: None,
        kind: IncidentKind::Approval,
        detail: "needs a grant".to_string(),
        agent_task_id: Some("task-1".to_string()),
    };
    let table = render_incidents(std::slice::from_ref(&incident));
    assert!(table.contains("open"), "{table}");
    assert!(!table.contains("acked"), "{table}");

    let mut acked = incident;
    acked.acked_at = Some(10);
    let table = render_incidents(&[acked]);
    assert!(table.contains("acked"), "{table}");
}

#[test]
fn duration_formats_scale_with_magnitude() {
    assert_eq!(format_duration(5), "5s");
    assert_eq!(format_duration(90), "1m30s");
    assert_eq!(format_duration(3_700), "1h1m");
}
