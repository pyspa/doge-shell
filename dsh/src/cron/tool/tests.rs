use super::*;
use tempfile::TempDir;

fn store() -> (TempDir, SqliteCronStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SqliteCronStore::open(&dir.path().join("cron")).expect("open");
    (dir, store)
}

fn request() -> CronToolRequest {
    CronToolRequest::default()
}

#[test]
fn create_registers_a_paused_shell_job_regardless_of_the_request() {
    let (_dir, store) = store();
    let request = CronToolRequest {
        name: Some("fetch".to_string()),
        schedule: Some("5m".to_string()),
        command: Some("git fetch --all".to_string()),
        ..request()
    };
    let value = create(&store, &request).expect("create");
    assert_eq!(value["job"], json!("fetch"));
    assert_eq!(value["paused"], json!(true));

    let job = store.get("fetch").expect("job exists");
    assert!(job.paused, "a job the tool creates must start paused");
    assert_eq!(job.command, "git fetch --all");
}

#[test]
fn create_defaults_a_name_from_the_command_when_none_is_given() {
    let (_dir, store) = store();
    let request = CronToolRequest {
        schedule: Some("5m".to_string()),
        command: Some("git fetch --all".to_string()),
        ..request()
    };
    let value = create(&store, &request).expect("create");
    assert_eq!(value["job"], json!("git"));
}

#[test]
fn create_without_a_schedule_is_a_clear_error() {
    let (_dir, store) = store();
    let request = CronToolRequest {
        command: Some("true".to_string()),
        ..request()
    };
    assert!(create(&store, &request).is_err());
}

#[test]
fn create_an_agent_job_needs_tokens_and_a_grant() {
    let (_dir, store) = store();
    let request = CronToolRequest {
        schedule: Some("5m".to_string()),
        agent: true,
        goal: Some("summarise new commits".to_string()),
        ..request()
    };
    let error = create(&store, &request).unwrap_err().to_string();
    assert!(error.contains("--read or --write"), "{error}");
}

#[test]
fn create_an_agent_job_with_a_grant_round_trips_it() {
    let (_dir, store) = store();
    let dir = tempfile::tempdir().unwrap();
    let request = CronToolRequest {
        name: Some("digest".to_string()),
        schedule: Some("0 9 * * mon-fri".to_string()),
        agent: true,
        goal: Some("summarise new commits".to_string()),
        tokens: Some("50000".to_string()),
        check: vec!["out/digest.md exists".to_string()],
        write: vec![dir.path().to_string_lossy().into_owned()],
        ..request()
    };
    create(&store, &request).expect("create");
    let job = store.get("digest").expect("job exists");
    let agent = job.agent.expect("agent spec");
    assert_eq!(agent.token_budget, 50_000);
    assert_eq!(agent.criteria, vec!["out/digest.md exists".to_string()]);
    assert_eq!(agent.grant.write_roots.len(), 1);
}

#[test]
fn create_refuses_a_duplicate_name_without_force() {
    let (_dir, store) = store();
    let request = CronToolRequest {
        name: Some("fetch".to_string()),
        schedule: Some("5m".to_string()),
        command: Some("git fetch".to_string()),
        ..request()
    };
    create(&store, &request).expect("first create");
    assert!(create(&store, &request).is_err());

    let forced = CronToolRequest {
        force: true,
        ..request
    };
    assert!(create(&store, &forced).is_ok());
}

#[test]
fn update_merges_onto_the_jobs_existing_grant() {
    let (_dir, store) = store();
    let dir = tempfile::tempdir().unwrap();
    create(
        &store,
        &CronToolRequest {
            name: Some("digest".to_string()),
            schedule: Some("5m".to_string()),
            agent: true,
            goal: Some("summarise".to_string()),
            tokens: Some("1000".to_string()),
            write: vec![dir.path().to_string_lossy().into_owned()],
            ..request()
        },
    )
    .expect("create");

    update(
        &store,
        &CronToolRequest {
            job: Some("digest".to_string()),
            check: vec!["done".to_string()],
            ..request()
        },
    )
    .expect("update");

    let job = store.get("digest").expect("job exists");
    let agent = job.agent.expect("agent spec");
    // The pre-existing write grant must survive a patch that only touched
    // `check` - `parse_edit` merges onto the job's current agent spec.
    assert_eq!(agent.grant.write_roots.len(), 1);
    assert_eq!(agent.criteria, vec!["done".to_string()]);
}

#[test]
fn update_without_naming_a_field_is_a_clear_error() {
    let (_dir, store) = store();
    create(
        &store,
        &CronToolRequest {
            name: Some("fetch".to_string()),
            schedule: Some("5m".to_string()),
            command: Some("git fetch".to_string()),
            ..request()
        },
    )
    .expect("create");

    let error = update(
        &store,
        &CronToolRequest {
            job: Some("fetch".to_string()),
            ..request()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("nothing to change"));
}

#[test]
fn pause_and_resume_round_trip() {
    let (_dir, store) = store();
    create(
        &store,
        &CronToolRequest {
            name: Some("fetch".to_string()),
            schedule: Some("5m".to_string()),
            command: Some("git fetch".to_string()),
            ..request()
        },
    )
    .expect("create");

    let req = CronToolRequest {
        job: Some("fetch".to_string()),
        ..request()
    };
    set_paused(&store, &req, true).expect("pause");
    assert!(store.get("fetch").unwrap().paused);
    set_paused(&store, &req, false).expect("resume");
    assert!(!store.get("fetch").unwrap().paused);
}

#[test]
fn remove_deletes_the_job() {
    let (_dir, store) = store();
    create(
        &store,
        &CronToolRequest {
            name: Some("fetch".to_string()),
            schedule: Some("5m".to_string()),
            command: Some("git fetch".to_string()),
            ..request()
        },
    )
    .expect("create");

    remove(
        &store,
        &CronToolRequest {
            job: Some("fetch".to_string()),
            ..request()
        },
    )
    .expect("remove");
    assert!(store.get("fetch").is_err());
}

#[test]
fn run_marks_the_job_due_without_spawning_anything() {
    let (_dir, store) = store();
    create(
        &store,
        &CronToolRequest {
            name: Some("fetch".to_string()),
            schedule: Some("@manual".to_string()),
            command: Some("git fetch".to_string()),
            ..request()
        },
    )
    .expect("create");

    let value = run(
        &store,
        &CronToolRequest {
            job: Some("fetch".to_string()),
            ..request()
        },
    )
    .expect("run");
    assert_eq!(value["action"], json!("run"));
}

#[test]
fn list_and_history_and_incidents_and_status_report_an_empty_store() {
    let (_dir, store) = store();
    assert_eq!(list(&store).unwrap(), json!([]));
    assert_eq!(history(&store, &request()).unwrap(), json!([]));
    assert_eq!(incidents(&store).unwrap(), json!([]));
    let health = status(&store).unwrap();
    assert_eq!(health["jobs"], json!(0));
}

#[test]
fn doctor_reports_on_an_empty_store() {
    let (_dir, store) = store();
    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    let value = doctor(&mut shell, &store).unwrap();
    // Not an exact match on every line: `doctor_report` also inspects the
    // real `config.lisp` for a deprecated `sched-add` call, which this test
    // does not control. Only what an empty store itself guarantees is
    // checked here.
    let lines = value["lines"].as_array().expect("lines array");
    assert!(
        lines.iter().any(|line| line.as_str() == Some("ok no-jobs")),
        "{lines:?}"
    );
}

#[test]
fn show_needs_a_job() {
    let (_dir, store) = store();
    assert!(show(&store, &request()).is_err());
}

#[test]
fn ack_needs_a_numeric_incident_id() {
    let (_dir, store) = store();
    let error = ack(
        &store,
        &CronToolRequest {
            incident_id: Some("not-a-number".to_string()),
            ..request()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("must be a number"));
}
