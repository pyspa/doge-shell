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

fn run_a_job_named(store: &SqliteCronStore, name: &str, cwd: &str, stdout: &str) {
    use dsh_types::cron::job::{RunOutcome, RunState, RunTrigger};

    create(
        store,
        &CronToolRequest {
            name: Some(name.to_string()),
            schedule: Some("1h".to_string()),
            command: Some("echo hi".to_string()),
            cwd: Some(cwd.to_string()),
            ..request()
        },
    )
    .expect("create");
    // The tool always registers a job paused (see `create`'s own doc
    // comment); a paused job is never claimed, `--paused` or not, so it has
    // to be resumed before this test can drive a run through it.
    store.set_paused(name, false, 0).expect("resume");
    store.trigger(name, 0).expect("trigger");
    let claimed = store
        .claim_due(0, "owner", 10, RunTrigger::Tick)
        .expect("claim");
    store.start(&claimed[0].run_id, 0).expect("start");
    store
        .complete(
            &claimed[0].run_id,
            &RunOutcome {
                state: RunState::Succeeded,
                stdout: stdout.to_string(),
                stderr: "trouble\n".to_string(),
                digest: Some(1),
                ..Default::default()
            },
            1,
        )
        .expect("complete");
}

/// The bug this guards against: with only `RunSelector::Id` available,
/// passing both `job` and `run` silently discarded `job` and returned
/// whichever job the `run` id actually belonged to - so a stale or
/// mistyped run id from a *different* job's history would silently return
/// that other job's recorded output instead of erroring.
#[test]
fn logs_with_both_job_and_run_refuses_a_run_from_a_different_job() {
    let (_dir, store) = store();
    run_a_job_named(&store, "digest", "/tmp", "digest output\n");
    run_a_job_named(&store, "other", "/tmp", "other output\n");

    let other_run = store
        .runs(&RunQuery {
            job: Some("other".to_string()),
            ..Default::default()
        })
        .expect("runs")
        .remove(0);

    let shell = crate::shell::Shell::new(crate::environment::Environment::new());
    let error = logs(
        &shell,
        &store,
        &CronToolRequest {
            job: Some("digest".to_string()),
            run: Some(other_run.id),
            ..request()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("no run with this id under job"),
        "{error}"
    );
}

#[test]
fn logs_returns_the_stored_streams() {
    let (_dir, store) = store();
    run_a_job_named(&store, "digest", "/tmp", "hello\n");

    let shell = crate::shell::Shell::new(crate::environment::Environment::new());
    let value = logs(
        &shell,
        &store,
        &CronToolRequest {
            job: Some("digest".to_string()),
            ..request()
        },
    )
    .expect("logs");
    assert_eq!(value["stdout"], json!("hello\n"));
    assert_eq!(value["stderr"], json!("trouble\n"));
}

/// The same boundary a write action's grant check enforces
/// (`grant_exceeds_task` in `dsh-builtin/src/chatgpt/tool/cron.rs`), applied
/// here to a read: a task must not be able to read an unrelated job's
/// output just because it happens to know the job's name.
#[test]
fn a_job_outside_the_calling_tasks_grant_is_refused() {
    use dsh_builtin::agent::AgentRuntime;
    use dsh_builtin::shell_capabilities::AgentTaskStore;
    use dsh_types::agent::{AgentTask, TaskGrant, TaskStatus};

    let (_dir, store) = store();
    run_a_job_named(&store, "digest", "/tmp", "hi\n");

    let agent_dir = tempfile::tempdir().expect("tempdir");
    // Granted only a narrower directory of its own - "/tmp" (the job's cwd)
    // is not inside it.
    let granted_root = agent_dir.path().join("workspace");
    std::fs::create_dir_all(&granted_root).expect("mkdir");

    let task = AgentTask {
        id: "task-1".to_string(),
        goal: "do something unrelated".to_string(),
        root: granted_root.clone(),
        status: TaskStatus::Running,
        grant: TaskGrant {
            read_roots: vec![granted_root.canonicalize().expect("canonicalize")],
            ..Default::default()
        },
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        token_budget: 100,
        tokens_used: 0,
        time_budget_ms: 1_000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    };
    let task_store = std::sync::Arc::new(
        crate::agent::SqliteTaskStore::open(&agent_dir.path().join("agent")).expect("open"),
    );
    task_store.save(&task, None).expect("save");

    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    shell.agent_runtime = Some(std::sync::Arc::new(parking_lot::Mutex::new(
        AgentRuntime::new(task, task_store),
    )));

    let error = logs(
        &shell,
        &store,
        &CronToolRequest {
            job: Some("digest".to_string()),
            ..request()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("outside this task's own"),
        "{error}"
    );
}

/// The regression this guards: `job_cwd_within` used to fail *open*
/// (`Err(_) => true`) on an unresolvable `cwd`, on the mistaken assumption
/// that some later step would still catch it - `logs` has no such later
/// step, so that let a task read any job whose directory happened to be
/// gone, regardless of its own grant.
#[test]
fn a_job_whose_cwd_no_longer_resolves_is_refused_under_a_task() {
    use dsh_builtin::agent::AgentRuntime;
    use dsh_builtin::shell_capabilities::AgentTaskStore;
    use dsh_types::agent::{AgentTask, TaskGrant, TaskStatus};

    let (_dir, store) = store();
    let gone = tempfile::tempdir().expect("tempdir");
    let gone_path = gone.path().join("job-cwd");
    std::fs::create_dir_all(&gone_path).expect("mkdir");
    run_a_job_named(&store, "digest", &gone_path.to_string_lossy(), "hi\n");
    // The job's own directory is gone by the time `logs` runs.
    std::fs::remove_dir_all(&gone_path).expect("rmdir");

    let agent_dir = tempfile::tempdir().expect("tempdir");
    let task = AgentTask {
        id: "task-1".to_string(),
        goal: "do something else".to_string(),
        root: agent_dir.path().to_path_buf(),
        status: TaskStatus::Running,
        // The grant is irrelevant here - an unresolvable `cwd` must be
        // refused regardless of what is granted, not read as "anything
        // goes".
        grant: TaskGrant::default(),
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        token_budget: 100,
        tokens_used: 0,
        time_budget_ms: 1_000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    };
    let task_store = std::sync::Arc::new(
        crate::agent::SqliteTaskStore::open(&agent_dir.path().join("agent")).expect("open"),
    );
    task_store.save(&task, None).expect("save");

    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    shell.agent_runtime = Some(std::sync::Arc::new(parking_lot::Mutex::new(
        AgentRuntime::new(task, task_store),
    )));

    let error = logs(
        &shell,
        &store,
        &CronToolRequest {
            job: Some("digest".to_string()),
            ..request()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("could not be resolved"),
        "an unresolvable cwd must be refused, not treated as in-grant, and must say so \
         distinctly from a genuine out-of-grant refusal: {error}"
    );
}
