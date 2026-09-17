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

/// The bug this guards against: an agent job's `command` is its goal in
/// full, which had no clamp at all - one such job blew the table out to
/// hundreds of columns. `cron show <job>`/`--json` still carry the untruncated
/// text, so nothing is lost, just not shown in the list.
#[test]
fn a_very_long_command_does_not_widen_the_list_table() {
    let mut j = job("probe");
    j.command = "x".repeat(200);
    let table = render_job_list(&[j], 0);
    assert!(!table.contains(&"x".repeat(200)), "{table}");
    assert!(table.contains("..."), "{table}");
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
fn a_failed_run_shows_both_its_reason_and_its_preview() {
    let mut r = run(RunState::Failed);
    r.reason = Some(RunReason::Timeout);
    r.preview = "stop reason: ran out of time".to_string();
    let table = render_history(&[r]);
    assert!(table.contains("timeout"), "{table}");
    assert!(table.contains("ran out of time"), "{table}");
}

#[test]
fn the_history_table_shows_a_run_id_to_pass_to_cron_logs() {
    let mut r = run(RunState::Succeeded);
    r.id = "abcdef1234567890".to_string();
    let table = render_history(&[r]);
    assert!(table.contains("abcdef12"), "{table}");
}

#[test]
fn a_very_long_preview_does_not_widen_the_detail_column() {
    let mut r = run(RunState::Succeeded);
    r.preview = "x".repeat(120);
    let table = render_history(&[r]);
    assert!(!table.contains(&"x".repeat(120)), "{table}");
    assert!(table.contains("..."), "{table}");
}

#[test]
fn a_run_with_nothing_to_say_shows_a_dash_not_an_empty_cell() {
    // The fixture's own `preview` ("hello") is non-empty, so clear it to hit
    // the case where a run has no reason and no preview.
    let mut r = run(RunState::Succeeded);
    r.preview.clear();
    assert_eq!(run_detail(&r), "-");
}

#[test]
fn render_run_output_labels_each_stream_and_says_when_one_is_empty() {
    let output = dsh_types::cron::job::RunOutput {
        run: run(RunState::Succeeded),
        stdout: "hello\n".to_string(),
        stderr: String::new(),
    };
    let text = render_run_output(&output, true, true);
    assert!(text.contains("--- stdout ---"), "{text}");
    assert!(text.contains("--- stderr ---"), "{text}");
    assert!(text.contains("hello"), "{text}");
    assert!(text.contains("(empty)"), "{text}");
}

#[test]
fn render_run_output_with_one_stream_selected_has_no_headers() {
    let output = dsh_types::cron::job::RunOutput {
        run: run(RunState::Succeeded),
        stdout: "hello\n".to_string(),
        stderr: "oops\n".to_string(),
    };
    let text = render_run_output(&output, true, false);
    assert!(!text.contains("---"), "{text}");
    assert!(text.contains("hello"), "{text}");
    assert!(!text.contains("oops"), "{text}");
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
