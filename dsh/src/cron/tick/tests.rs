use super::*;
use dsh_types::cron::job::JobKind;
use dsh_types::schedule::{NotifyPolicy, parse_schedule};
use std::collections::HashMap;

fn store() -> (tempfile::TempDir, SqliteCronStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteCronStore::open(&dir.path().join("cron")).unwrap();
    (dir, store)
}

fn spec(name: &str) -> dsh_types::cron::job::CronJobSpec {
    dsh_types::cron::job::CronJobSpec {
        name: name.to_string(),
        schedule: parse_schedule("1h").unwrap(),
        schedule_spec: "1h".to_string(),
        kind: JobKind::Sh,
        command: "true".to_string(),
        agent: None,
        cwd: "/tmp".to_string(),
        notify: NotifyPolicy::default(),
        timeout_secs: 60,
        catchup_secs: 3600,
        paused: false,
    }
}

#[test]
fn an_idle_store_starts_nothing() {
    let (_dir, store) = store();
    let report = run_once(&store, "owner", MAX_PER_TICK, RunTrigger::Tick, 1_000).unwrap();
    assert!(report.started.is_empty());
    assert!(report.spawn_failed.is_empty());
}

/// The real end-to-end path: claim a due job, actually spawn the child (this
/// dsh binary itself), and see it recorded as `running` shortly after.
#[test]
fn a_due_job_gets_a_real_child_started_for_it() {
    let (_dir, store) = store();
    store
        .create(&spec("probe"), &HashMap::new(), 1_000, false)
        .unwrap();
    store.trigger("probe", 1_000).unwrap();

    let report = run_once(&store, "owner", MAX_PER_TICK, RunTrigger::Tick, 1_000).unwrap();
    assert_eq!(report.started, vec!["probe"]);
    assert!(report.spawn_failed.is_empty());

    // Give the spawned `cron run-job` a moment to reach the store on its own
    // connection and move the run past `queued`.
    for _ in 0..50 {
        let job = store.get("probe").unwrap();
        if job.running || job.run_count > 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("spawned run-job child never touched the store");
}

#[test]
fn owner_id_is_stable_in_shape() {
    let id = owner_id();
    assert_eq!(id.matches(':').count(), 2, "{id}");
}
