use super::*;
use dsh_types::cron::job::{AgentJobSpec, RunOutcome, RunSelector, RunState, RunTrigger};
use tempfile::TempDir;

const NOW: i64 = 1_717_234_200;

/// The store creates its own directory with the permissions it insists on, so
/// tests point it at a path *inside* the temporary directory rather than at
/// the temporary directory itself, whose mode is the harness's business.
fn root(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("cron")
}

fn store() -> (TempDir, SqliteCronStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SqliteCronStore::open(&root(&dir)).expect("open");
    (dir, store)
}

fn spec(name: &str, schedule: &str) -> CronJobSpec {
    CronJobSpec {
        name: name.to_string(),
        schedule: parse_schedule(schedule).expect("schedule"),
        schedule_spec: schedule.to_string(),
        kind: JobKind::Sh,
        command: "echo hello".to_string(),
        agent: None,
        cwd: "/tmp".to_string(),
        notify: NotifyPolicy::default(),
        timeout_secs: 60,
        catchup_secs: 3600,
        paused: false,
    }
}

fn env() -> HashMap<String, String> {
    HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])
}

fn outcome(state: RunState) -> RunOutcome {
    RunOutcome {
        state,
        stdout: "hello\n".to_string(),
        digest: Some(7),
        ..Default::default()
    }
}

/// Makes a job due at `at` without waiting for its schedule.
///
/// `at` matters: the slot a run belongs to is its `scheduled_for`, and the
/// store refuses two runs for one slot. A loop that re-triggers at the same
/// instant is asking for the same slot twice, and correctly gets nothing.
fn make_due(store: &SqliteCronStore, name: &str, at: i64) {
    store.trigger(name, at).expect("trigger");
}

fn raw(dir: &TempDir) -> Connection {
    Connection::open(root(dir).join("jobs.sqlite3")).expect("raw connection")
}

#[test]
fn opening_twice_is_idempotent() {
    let (dir, store) = store();
    store
        .create(&spec("probe", "5m"), &env(), NOW, false)
        .unwrap();
    drop(store);

    let reopened = SqliteCronStore::open(&root(&dir)).expect("reopen");
    assert_eq!(reopened.list().unwrap().len(), 1);
}

/// An external tick in the system crontab can easily be an older binary than
/// the shell that upgraded the store. Writing the old shape into a new schema
/// is worse than not running at all.
#[test]
fn a_store_from_a_newer_dsh_is_refused() {
    let (dir, store) = store();
    drop(store);
    raw(&dir)
        .pragma_update(None, "user_version", 99_i64)
        .unwrap();

    let error = SqliteCronStore::open(&root(&dir)).unwrap_err().to_string();
    assert!(
        error.contains("newer dsh") || error.contains("schema 99"),
        "{error}"
    );
}

#[test]
fn a_job_round_trips_through_the_store() {
    let (_dir, store) = store();
    let id = store
        .create(&spec("probe", "5m"), &env(), NOW, false)
        .unwrap();

    let job = store.get("probe").unwrap();
    assert_eq!(job.id, id);
    assert_eq!(job.name, "probe");
    assert_eq!(job.kind, JobKind::Sh);
    assert_eq!(job.command, "echo hello");
    assert_eq!(job.schedule_spec, "5m");
    assert_eq!(job.next_run_at, Some(NOW + 300));
    assert!(!job.paused);
    assert_eq!(job.state_label(), "ok");

    // Both spellings of a selector reach the same job.
    assert_eq!(store.get(&id.to_string()).unwrap().name, "probe");
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.delete("probe").unwrap(), "probe");
    assert!(store.get("probe").is_err());
}

#[test]
fn an_agent_job_keeps_its_grant() {
    let (_dir, store) = store();
    let mut job = spec("digest", "@daily");
    job.kind = JobKind::Ai;
    job.command = "summarise today's commits".to_string();
    job.agent = Some(AgentJobSpec {
        criteria: vec!["a summary file exists".to_string()],
        token_budget: 50_000,
        time_budget_secs: 900,
        max_tokens_per_day: Some(200_000),
        ..Default::default()
    });
    store.create(&job, &env(), NOW, false).unwrap();

    let stored = store.get("digest").unwrap();
    assert_eq!(stored.kind, JobKind::Ai);
    assert_eq!(stored.command, "summarise today's commits");
    let agent = stored.agent.expect("payload");
    assert_eq!(agent.token_budget, 50_000);
    assert_eq!(agent.max_tokens_per_day, Some(200_000));
}

/// A typo at the prompt must not silently replace a working job.
#[test]
fn a_duplicate_name_needs_force() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5m"), &env(), NOW, false)
        .unwrap();

    let error = store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--force"), "{error}");
    assert_eq!(store.get("probe").unwrap().schedule_spec, "5m");

    store
        .create(&spec("probe", "1h"), &env(), NOW, true)
        .unwrap();
    assert_eq!(store.get("probe").unwrap().schedule_spec, "1h");
}

/// `config.lisp` is evaluated on every startup, so the `create` rule would
/// make the second launch an error - and an error there aborts the rest of
/// the file, taking the user's aliases and PATH with it.
/// The whole reason `cron-add` upserts rather than erroring: `config.lisp`
/// calls it on every launch. `INSERT OR REPLACE` (SQLite's delete-then-insert)
/// used to satisfy that by cascading away the job's own run history and
/// handing it a new id each time - `ON CONFLICT DO UPDATE` is what actually
/// keeps the id and the history a person is looking at in `cron history`.
#[test]
fn upserting_an_existing_job_preserves_its_id_and_run_history() {
    let (_dir, store) = store();
    let id = store.upsert(&spec("probe", "5m"), &env(), NOW).unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store
        .complete(&claimed[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();
    assert_eq!(store.get("probe").unwrap().run_count, 1);

    // Re-declare the same job, as config.lisp would on the next launch.
    let same_id = store
        .upsert(&spec("probe", "10m"), &env(), NOW + 100)
        .unwrap();

    assert_eq!(same_id, id, "upsert must not hand out a new id");
    let job = store.get("probe").unwrap();
    assert_eq!(job.id, id);
    assert_eq!(
        job.schedule_spec, "10m",
        "the new definition must take effect"
    );
    assert_eq!(job.run_count, 1, "run history must survive the upsert");
    assert_eq!(store.runs(&RunQuery::default()).unwrap().len(), 1);
}

#[test]
fn upsert_is_safe_to_repeat() {
    let (_dir, store) = store();
    for _ in 0..3 {
        store.upsert(&spec("probe", "5m"), &env(), NOW).unwrap();
    }
    assert_eq!(store.list().unwrap().len(), 1);
}

/// The bug this guards against: `config.lisp` calls `cron-add` (`upsert`) on
/// every launch, and its spec's `paused` field is always `false` - it has no
/// way to say "leave whatever pause state this job is already in alone". An
/// earlier version applied `spec.paused` on every upsert regardless, so
/// simply restarting the shell (or running `reload`) silently resumed any
/// job the user had paused with `cron pause`.
#[test]
fn upserting_a_paused_job_does_not_silently_resume_it() {
    let (_dir, store) = store();
    store.upsert(&spec("probe", "5m"), &env(), NOW).unwrap();
    store.set_paused("probe", true, NOW).unwrap();
    assert!(store.get("probe").unwrap().paused);

    // Re-declare the same job, exactly as config.lisp does on every launch.
    store
        .upsert(&spec("probe", "5m"), &env(), NOW + 10)
        .unwrap();

    let job = store.get("probe").unwrap();
    assert!(job.paused, "an upsert must not resume a paused job");
    assert_eq!(job.next_run_at, None);
}

/// `cron add --force` (`create`), unlike `upsert`, is a person's own explicit
/// command - its `--paused`/no-`--paused` spelling is expected to take
/// effect the same as a fresh `cron add`.
#[test]
fn force_creating_over_a_paused_job_applies_the_new_specs_pause_state() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5m"), &env(), NOW, false)
        .unwrap();
    store.set_paused("probe", true, NOW).unwrap();
    assert!(store.get("probe").unwrap().paused);

    let mut replacement = spec("probe", "5m");
    replacement.paused = false;
    store.create(&replacement, &env(), NOW + 10, true).unwrap();

    assert!(!store.get("probe").unwrap().paused);
}

/// The single most important property in the whole feature: two drivers, one
/// execution. Real connections to one database file, not a mocked lock.
#[test]
fn only_one_driver_can_claim_a_due_job() {
    let (dir, first) = store();
    first
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&first, "probe", NOW);

    let second = SqliteCronStore::open(&root(&dir)).expect("second driver");

    let mine = first
        .claim_due(NOW, "host:1:a", 10, RunTrigger::Tick)
        .unwrap();
    let theirs = second
        .claim_due(NOW, "host:2:b", 10, RunTrigger::Tick)
        .unwrap();

    assert_eq!(mine.len() + theirs.len(), 1, "the slot ran twice");
    let claimed = mine.into_iter().chain(theirs).next().unwrap();
    assert_eq!(claimed.job_name, "probe");
    assert_eq!(claimed.command, "echo hello");
    assert_eq!(
        claimed.env.get("PATH").map(String::as_str),
        Some("/usr/bin:/bin")
    );
}

/// Claiming advances the slot in the same transaction. Without that a job
/// stays due and every scan claims it again, whatever the run did.
#[test]
fn claiming_advances_the_next_slot() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);

    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].scheduled_for, NOW);

    let job = store.get("probe").unwrap();
    assert_eq!(job.next_run_at, Some(NOW + 3600));
    assert!(job.running, "the claim should show as running");
}

/// The second net, independent of the claim: a fall-back that repeats an hour
/// must not run the same slot twice.
#[test]
fn the_same_slot_cannot_produce_two_runs() {
    let (dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);

    let first = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    assert_eq!(first.len(), 1);
    store
        .complete(&first[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();

    // Put the clock back on the slot that already ran.
    raw(&dir)
        .execute("UPDATE jobs SET next_run_at = ?1", [NOW])
        .unwrap();

    let again = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    assert!(again.is_empty(), "the same slot was claimed twice");
    assert!(
        !store.get("probe").unwrap().running,
        "a rejected claim must release the job"
    );
}

/// A process that dies mid-run cannot release its own claim, so the claim has
/// a deadline and the next scan takes it back.
#[test]
fn an_expired_lease_is_reclaimed() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5s"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);

    let held = store
        .claim_due(NOW, "dead-owner", 10, RunTrigger::Tick)
        .unwrap();
    assert_eq!(held.len(), 1);
    assert!(
        store
            .claim_due(NOW, "other", 10, RunTrigger::Tick)
            .unwrap()
            .is_empty()
    );

    // Twice the 60s timeout, plus a second.
    let later = NOW + 121;
    let taken = store
        .claim_due(later, "other", 10, RunTrigger::Tick)
        .unwrap();
    assert_eq!(taken.len(), 1, "the lease never expired");
}

/// The bug this guards against: `reap_expired_leases` used to close an
/// abandoned run out as an ordinary `failed`/`timeout` without charging the
/// job for it at all - `run_count`/`fail_count`/`consecutive_failures` never
/// moved, so a job whose process kept dying mid-run could do so forever with
/// nothing in `cron list`/`cron status` ever calling it out, and
/// `IncidentKind::LeaseLost` (built for exactly this case) never fired.
#[test]
fn a_reaped_lease_counts_as_a_failure_and_opens_a_lease_lost_incident() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5s"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store
        .claim_due(NOW, "dead-owner", 10, RunTrigger::Tick)
        .unwrap();
    assert_eq!(claimed.len(), 1);

    // Twice the (60s-floored) lease, plus a second: long enough that the
    // claim is treated as abandoned rather than merely slow.
    let later = NOW + 121;
    let reaped = store.reap_expired_leases(later).unwrap();
    assert_eq!(reaped, 1);

    let job = store.get("probe").unwrap();
    assert_eq!(
        job.run_count, 1,
        "an abandoned run must still count as a run"
    );
    assert_eq!(job.fail_count, 1);
    assert_eq!(job.consecutive_failures, 1);

    let runs = store.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs[0].state, RunState::Failed);

    let open = store.incidents(true, 10).unwrap();
    assert_eq!(
        open.len(),
        1,
        "a lease loss must be visible in `cron incidents`"
    );
    assert_eq!(open[0].kind, IncidentKind::LeaseLost);
    assert_eq!(open[0].job_name.as_deref(), Some("probe"));
}

#[test]
fn a_paused_or_blocked_job_is_never_claimed() {
    let (dir, store) = store();
    store
        .create(&spec("paused", "1h"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("blocked", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "paused", NOW);
    make_due(&store, "blocked", NOW);
    store.set_paused("paused", true, NOW).unwrap();
    raw(&dir)
        .execute("UPDATE jobs SET blocked = 1 WHERE name = 'blocked'", [])
        .unwrap();

    assert!(
        store
            .claim_due(NOW, "owner", 10, RunTrigger::Tick)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.get("paused").unwrap().state_label(), "paused");
    assert_eq!(store.get("blocked").unwrap().state_label(), "blocked");
}

/// Resuming re-bases rather than restoring the old slot: the point of pausing
/// is to not owe the runs that elapsed meanwhile.
#[test]
fn resuming_rebases_the_next_slot() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5m"), &env(), NOW, false)
        .unwrap();
    store.set_paused("probe", true, NOW).unwrap();
    assert_eq!(store.get("probe").unwrap().next_run_at, None);

    let much_later = NOW + 86_400;
    store.set_paused("probe", false, much_later).unwrap();
    assert_eq!(
        store.get("probe").unwrap().next_run_at,
        Some(much_later + 300)
    );
}

#[test]
fn pausing_everything_and_resuming_rebases_every_job() {
    let (_dir, store) = store();
    store
        .create(&spec("one", "5m"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("two", "1h"), &env(), NOW, false)
        .unwrap();

    assert_eq!(store.set_all_paused(true, NOW).unwrap(), 2);
    assert!(
        store
            .list()
            .unwrap()
            .iter()
            .all(|job| job.next_run_at.is_none())
    );

    let later = NOW + 86_400;
    store.set_all_paused(false, later).unwrap();
    assert_eq!(store.get("one").unwrap().next_run_at, Some(later + 300));
    assert_eq!(store.get("two").unwrap().next_run_at, Some(later + 3600));
}

/// A job paused on its own (`cron pause <job>`) must not come back just
/// because someone ran a global `cron pause` and `cron resume` - the two
/// pause mechanisms share one `enabled` column, so without a separate memory
/// of "this one was paused on purpose", a global resume could not tell it
/// apart from a job the master switch itself had disabled.
#[test]
fn an_individually_paused_job_stays_paused_through_a_global_pause_and_resume() {
    let (_dir, store) = store();
    store
        .create(&spec("kept-off", "5m"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("normal", "5m"), &env(), NOW, false)
        .unwrap();

    store.set_paused("kept-off", true, NOW).unwrap();
    assert!(store.get("kept-off").unwrap().paused);

    assert_eq!(store.set_all_paused(true, NOW).unwrap(), 2);
    let later = NOW + 3_600;
    let resumed = store.set_all_paused(false, later).unwrap();

    assert!(
        store.get("kept-off").unwrap().paused,
        "a job individually paused before the global pause must still be paused after resume"
    );
    assert!(!store.get("normal").unwrap().paused);
    assert_eq!(
        resumed, 1,
        "only the job that was not individually paused should count as resumed"
    );
}

/// A job registered `--paused` was paused on purpose, exactly like one that
/// `cron pause <job>` turned off - so a global `cron resume` must leave it
/// alone too. `cron_manage`'s always-create-paused rule rests on this: it is
/// what stops one ordinary `cron resume` from arming every job an agent
/// created, none of which anyone has looked at yet.
#[test]
fn a_job_created_paused_stays_paused_through_a_global_resume() {
    let (_dir, store) = store();
    let mut paused_spec = spec("born-paused", "5m");
    paused_spec.paused = true;
    store.create(&paused_spec, &env(), NOW, false).unwrap();
    store
        .create(&spec("normal", "5m"), &env(), NOW, false)
        .unwrap();

    assert!(store.get("born-paused").unwrap().paused);

    store.set_all_paused(true, NOW).unwrap();
    let resumed = store.set_all_paused(false, NOW + 3_600).unwrap();

    assert!(
        store.get("born-paused").unwrap().paused,
        "a job created --paused must not be started by a global resume"
    );
    assert!(!store.get("normal").unwrap().paused);
    assert_eq!(resumed, 1);
}

/// The other direction of the same column: `cron add --force` over a job
/// that was born `--paused` has to clear that memory, not just `enabled` -
/// otherwise the replacement looks live but a later global resume would still
/// skip it, as if it were paused on purpose.
#[test]
fn force_re_adding_a_born_paused_job_clears_its_individual_pause() {
    let (_dir, store) = store();
    let mut paused_spec = spec("toggle", "5m");
    paused_spec.paused = true;
    store.create(&paused_spec, &env(), NOW, false).unwrap();
    assert!(store.get("toggle").unwrap().paused);

    store
        .create(&spec("toggle", "5m"), &env(), NOW, true)
        .unwrap();
    assert!(!store.get("toggle").unwrap().paused);

    store.set_all_paused(true, NOW).unwrap();
    assert_eq!(
        store.set_all_paused(false, NOW + 3_600).unwrap(),
        1,
        "the replacement is an ordinary job again, so a global resume must take it"
    );
}

/// The bug this guards against: a global `cron resume` used to call
/// `rebase_all` unconditionally, which recomputed *every* enabled job's
/// `next_run_at` from `now` - not just the one(s) actually being resumed. A
/// job that was never paused has a countdown already in progress; resuming
/// an unrelated job in the same call must not reset (and typically delay)
/// it.
#[test]
fn resuming_one_job_does_not_rebase_an_unrelated_job_that_was_never_paused() {
    let (_dir, store) = store();
    store
        .create(&spec("to-resume", "1h"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("running", "1h"), &env(), NOW, false)
        .unwrap();
    // Both start off, as if from an earlier global pause; "running" then
    // comes back on its own (as `cron resume running` would), leaving
    // "to-resume" as the only job a later *global* resume actually needs to
    // touch.
    store.set_all_paused(true, NOW).unwrap();
    store.set_paused("running", false, NOW).unwrap();

    // "running" is partway through its interval, due soon - not a fresh
    // full hour away from `later`, which is what a rebase would produce.
    let soon = NOW + 120;
    store.trigger("running", soon).unwrap();
    assert_eq!(store.get("running").unwrap().next_run_at, Some(soon));

    let later = NOW + 1_800;
    let resumed = store.set_all_paused(false, later).unwrap();

    assert_eq!(resumed, 1, "only the job that was actually off is resumed");
    assert_eq!(
        store.get("running").unwrap().next_run_at,
        Some(soon),
        "a job that was already enabled must not have its schedule reset by an unrelated resume"
    );
    assert_eq!(
        store.get("to-resume").unwrap().next_run_at,
        Some(later + 3600)
    );
}

/// A row whose `schedule_spec` cannot be parsed (only reachable through data
/// corruption, since the store validates it on every write) must not stop
/// every job after it, by id order, from being rebased on a global resume -
/// that used to leave them with `next_run_at` stuck at `NULL` forever for a
/// problem that was never theirs.
#[test]
fn a_corrupt_schedule_does_not_block_the_rest_of_a_global_resume() {
    let (_dir, store) = store();
    store
        .create(&spec("corrupt", "5m"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("healthy", "5m"), &env(), NOW, false)
        .unwrap();

    store.set_all_paused(true, NOW).unwrap();
    {
        let connection = store.connection.lock();
        connection
            .execute(
                "UPDATE jobs SET schedule_spec = 'not a schedule' WHERE name = 'corrupt'",
                [],
            )
            .unwrap();
    }

    let later = NOW + 3_600;
    assert!(store.set_all_paused(false, later).is_ok());
    assert_eq!(
        store.get("healthy").unwrap().next_run_at,
        Some(later + 300),
        "a job after the corrupt one, by id order, must still be rebased"
    );
}

#[test]
fn a_run_records_what_happened() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();

    let started = store.start(&claimed[0].run_id, NOW).unwrap();
    assert_eq!(started.job_name, "probe");
    // Starting twice is a bug in the caller, not a second run.
    assert!(store.start(&claimed[0].run_id, NOW).is_err());

    store
        .complete(&claimed[0].run_id, &outcome(RunState::Succeeded), NOW + 5)
        .unwrap();

    let runs = store.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Succeeded);
    assert_eq!(runs[0].preview, "hello");
    assert!(!store.get("probe").unwrap().running);
    assert_eq!(store.get("probe").unwrap().run_count, 1);
}

/// A first run has nothing to differ from. Reporting it as changed would make
/// every job on `--on change` announce itself once, for nothing.
#[test]
fn the_first_run_is_never_a_change() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    let mut at = NOW;
    let mut states = Vec::new();
    for digest in [7_u64, 7, 9] {
        make_due(&store, "probe", at);
        let claimed = store.claim_due(at, "owner", 10, RunTrigger::Tick).unwrap();
        let mut result = outcome(RunState::Succeeded);
        result.digest = Some(digest);
        store.complete(&claimed[0].run_id, &result, at + 1).unwrap();
        at += 7200;
        states.push(store.runs(&RunQuery::default()).unwrap()[0].changed);
    }
    assert_eq!(states, vec![false, false, true]);
}

/// A skipped run is not a failure: another agent task holding the lock must
/// not move `fail_count` or trip `--on failure`.
#[test]
fn a_skipped_run_does_not_count_as_a_failure() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();

    let mut result = outcome(RunState::Skipped);
    result.reason = Some(RunReason::AgentBusy);
    result.digest = None;
    store
        .complete(&claimed[0].run_id, &result, NOW + 1)
        .unwrap();

    let job = store.get("probe").unwrap();
    assert_eq!(job.fail_count, 0);
    assert_eq!(job.consecutive_failures, 0);
    assert_eq!(job.state_label(), "ok");
    assert!(store.incidents(true, 20).unwrap().is_empty());
}

/// An approval the job was never granted is the same answer every tick, so it
/// stops the job rather than burning a budget rediscovering it.
#[test]
fn an_approval_incident_blocks_the_job_until_acknowledged() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();

    let mut result = outcome(RunState::NeedsApproval);
    result.agent_task_id = Some("task-1".to_string());
    store
        .complete(&claimed[0].run_id, &result, NOW + 1)
        .unwrap();

    let open = store.incidents(true, 20).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, IncidentKind::Approval);
    assert_eq!(open[0].agent_task_id.as_deref(), Some("task-1"));
    assert!(store.get("probe").unwrap().blocked);

    make_due(&store, "probe", NOW + 2);
    assert!(
        store
            .claim_due(NOW + 2, "owner", 10, RunTrigger::Tick)
            .unwrap()
            .is_empty(),
        "a blocked job kept firing"
    );

    store.ack_incident(open[0].id, NOW + 3).unwrap();
    assert!(!store.get("probe").unwrap().blocked);
    assert!(store.incidents(true, 20).unwrap().is_empty());
}

/// One report per problem. Without the dedupe a stuck job files one incident
/// per tick and buries every other job's.
#[test]
fn the_same_problem_opens_one_incident() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    for offset in 0..3 {
        store
            .open_incident(
                Some(store.get("probe").unwrap().id),
                IncidentKind::Config,
                "no API key",
                None,
                NOW + offset,
            )
            .unwrap();
    }
    assert_eq!(store.incidents(true, 20).unwrap().len(), 1);
}

/// A network blip is a failed run, not an incident - until it stops looking
/// like a blip.
#[test]
fn a_streak_of_failures_becomes_an_incident() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    let mut at = NOW;
    for round in 0..3 {
        make_due(&store, "probe", at);
        let claimed = store.claim_due(at, "owner", 10, RunTrigger::Tick).unwrap();
        let mut result = outcome(RunState::Failed);
        result.reason = Some(RunReason::Transient);
        store.complete(&claimed[0].run_id, &result, at + 1).unwrap();
        let open = store.incidents(true, 20).unwrap();
        if round < 2 {
            assert!(open.is_empty(), "round {round} reported too early");
        } else {
            assert_eq!(open.len(), 1);
            assert_eq!(open[0].kind, IncidentKind::Failing);
        }
        at += 7200;
    }

    // Recovering retires the report on its own - nobody has to acknowledge a
    // problem that went away.
    make_due(&store, "probe", at);
    let claimed = store.claim_due(at, "owner", 10, RunTrigger::Tick).unwrap();
    store
        .complete(&claimed[0].run_id, &outcome(RunState::Succeeded), at + 1)
        .unwrap();
    assert!(store.incidents(true, 20).unwrap().is_empty());
    assert_eq!(store.get("probe").unwrap().consecutive_failures, 0);
}

#[test]
fn history_can_be_narrowed_to_failures() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    let mut at = NOW;
    for state in [RunState::Succeeded, RunState::Failed, RunState::Succeeded] {
        make_due(&store, "probe", at);
        let claimed = store.claim_due(at, "owner", 10, RunTrigger::Tick).unwrap();
        store
            .complete(&claimed[0].run_id, &outcome(state), at + 1)
            .unwrap();
        at += 7200;
    }

    assert_eq!(store.runs(&RunQuery::default()).unwrap().len(), 3);
    let failed = store
        .runs(&RunQuery {
            failed_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].state, RunState::Failed);
}

/// The notepad has to sit outside the agent state directory: `task_file_allowed`
/// refuses that whole subtree, and the notepad must be writable by the agent
/// task whose job owns it.
#[test]
fn a_notepad_round_trips_and_lives_outside_the_agent_state() {
    let (dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    assert_eq!(store.notepad("probe").unwrap(), "");
    store.set_notepad("probe", "remember this").unwrap();
    assert_eq!(store.notepad("probe").unwrap(), "remember this");

    let path = store.notepad_path("probe");
    assert!(path.starts_with(root(&dir)), "{}", path.display());
    assert!(!path.to_string_lossy().contains("/agent/"));

    // Deleting the job takes its memory with it.
    store.delete("probe").unwrap();
    assert!(!path.exists());
}

/// The notepad is prepended to every run's goal, so an unbounded one is a
/// quietly growing bill on a schedule nobody is watching.
#[test]
fn a_notepad_is_capped() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    store
        .set_notepad("probe", &"x".repeat(MAX_NOTEPAD_BYTES * 2))
        .unwrap();
    assert!(store.notepad("probe").unwrap().len() <= MAX_NOTEPAD_BYTES);
}

/// A name with a path separator in it must not point the write grant at a
/// directory of the job's choosing.
#[test]
fn a_notepad_path_cannot_escape_its_directory() {
    let (dir, store) = store();
    let path = store.notepad_path("../../etc/passwd");
    assert_eq!(path.parent().unwrap(), root(&dir).join("notepad"));
}

#[test]
fn health_counts_what_the_status_line_shows() {
    let (_dir, store) = store();
    store
        .create(&spec("one", "5m"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("two", "5m"), &env(), NOW, false)
        .unwrap();
    store.set_paused("two", true, NOW).unwrap();
    make_due(&store, "one", NOW);

    let health = store.health(NOW).unwrap();
    assert_eq!(health.total, 2);
    assert_eq!(health.paused, 1);
    assert_eq!(health.overdue, 1);
    assert_eq!(health.failing, 0);
    assert_eq!(health.open_incidents, 0);
    assert_eq!(health.last_run_at, None);
}

#[test]
fn the_next_due_time_is_the_earliest_unpaused_job() {
    let (_dir, store) = store();
    assert_eq!(store.next_due_at().unwrap(), None);

    store
        .create(&spec("slow", "1h"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("fast", "5m"), &env(), NOW, false)
        .unwrap();
    assert_eq!(store.next_due_at().unwrap(), Some(NOW + 300));

    store.set_paused("fast", true, NOW).unwrap();
    assert_eq!(store.next_due_at().unwrap(), Some(NOW + 3600));
}

/// A five-minute schedule at ten thousand tokens a run is an unbounded bill,
/// so the ceiling is enforced against what was actually spent.
#[test]
fn spent_tokens_are_summed_over_the_window() {
    let (_dir, store) = store();
    let id = store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    let mut at = NOW;
    for tokens in [1_000_u64, 2_500] {
        make_due(&store, "probe", at);
        let claimed = store.claim_due(at, "owner", 10, RunTrigger::Tick).unwrap();
        let mut result = outcome(RunState::Succeeded);
        result.tokens_used = tokens;
        store.complete(&claimed[0].run_id, &result, at + 1).unwrap();
        at += 7200;
    }

    assert_eq!(store.tokens_used_since(id, NOW).unwrap(), 3_500);
    assert_eq!(store.tokens_used_since(id, at).unwrap(), 0);
}

#[test]
fn editing_one_field_leaves_the_rest_alone() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5m"), &env(), NOW, false)
        .unwrap();

    assert!(store.patch("probe", &CronJobPatch::default(), NOW).is_err());

    store
        .patch(
            "probe",
            &CronJobPatch {
                command: Some("echo goodbye".to_string()),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();
    let job = store.get("probe").unwrap();
    assert_eq!(job.command, "echo goodbye");
    assert_eq!(job.schedule_spec, "5m");
    assert_eq!(job.timeout_secs, 60);
}

/// The next slot has to move with the schedule, or the job keeps one more
/// appointment under the rule that was just replaced.
#[test]
fn editing_the_schedule_moves_the_next_slot() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    assert_eq!(store.get("probe").unwrap().next_run_at, Some(NOW + 3600));

    let schedule = parse_schedule("5m").unwrap();
    store
        .patch(
            "probe",
            &CronJobPatch {
                schedule: Some((schedule, "5m".to_string())),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();
    assert_eq!(store.get("probe").unwrap().next_run_at, Some(NOW + 300));
}

/// The bug this guards against: editing a paused job's schedule used to
/// compute and write a concrete `next_run_at` anyway - the row stayed
/// `enabled = 0` so it was never actually claimed, but `cron list --json`/
/// `cron_manage(show)` expose the column raw, so a paused job briefly looked
/// like it had a real next run scheduled.
#[test]
fn editing_the_schedule_of_a_paused_job_does_not_set_a_next_run_at() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    store.set_paused("probe", true, NOW).unwrap();
    assert_eq!(store.get("probe").unwrap().next_run_at, None);

    let schedule = parse_schedule("5m").unwrap();
    store
        .patch(
            "probe",
            &CronJobPatch {
                schedule: Some((schedule, "5m".to_string())),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();

    let job = store.get("probe").unwrap();
    assert_eq!(
        job.next_run_at, None,
        "a paused job must stay next_run_at=NULL"
    );
    assert!(job.paused);
    assert_eq!(
        job.schedule_spec, "5m",
        "the new schedule must still take effect"
    );
}

/// The bug this guards against: `build_spec` caps a new interval job's
/// timeout to its own interval ("an interval job that outlives its own
/// interval would starve its own next run"), but editing only the schedule
/// left a stale, longer timeout in place - reproducing on an edit exactly
/// the starvation `build_spec` exists to prevent at create time.
#[test]
fn editing_only_the_schedule_reclamps_a_now_too_long_timeout() {
    let (_dir, store) = store();
    // The raw `spec()` helper (unlike `build_spec`) sets `timeout_secs: 60`
    // regardless of the interval, so a "1h" job starts with a 60s timeout.
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    assert_eq!(store.get("probe").unwrap().timeout_secs, 60);

    let schedule = parse_schedule("30s").unwrap();
    store
        .patch(
            "probe",
            &CronJobPatch {
                schedule: Some((schedule, "30s".to_string())),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();

    assert_eq!(
        store.get("probe").unwrap().timeout_secs,
        30,
        "a 60s timeout must not survive a schedule edit to a 30s interval"
    );
}

/// The reclamp matches `build_spec`'s own rule exactly: the interval is a
/// hard ceiling even against an explicit `--timeout` in the same edit, the
/// same way `cron add --timeout 1h '30s' cmd` is silently capped to 30s at
/// creation time rather than honouring the longer value.
#[test]
fn an_explicit_timeout_in_the_same_edit_is_still_capped_to_the_new_interval() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    let schedule = parse_schedule("30s").unwrap();
    store
        .patch(
            "probe",
            &CronJobPatch {
                schedule: Some((schedule, "30s".to_string())),
                timeout_secs: Some(3_600),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();

    assert_eq!(store.get("probe").unwrap().timeout_secs, 30);
}

/// A schedule edit alone must not touch a timeout that already fits: capping
/// is a ceiling, not a floor or a forced reset.
#[test]
fn a_timeout_already_within_the_new_interval_is_left_alone() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    store
        .patch(
            "probe",
            &CronJobPatch {
                timeout_secs: Some(10),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();

    let schedule = parse_schedule("5m").unwrap();
    store
        .patch(
            "probe",
            &CronJobPatch {
                schedule: Some((schedule, "5m".to_string())),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();

    assert_eq!(store.get("probe").unwrap().timeout_secs, 10);
}

/// The bug this guards against: the `timeout_secs` reclamp above shrinks the
/// `jobs` column, but an AI job's watchdog is armed from its own copy inside
/// `payload` (`AgentJobSpec.time_budget_secs`) - `parse_edit` only resyncs
/// that copy when `--timeout` or a grant flag is also named in the edit. A
/// `--schedule`-only edit named neither, so without mirroring the clamp into
/// the payload too, the lease would shrink while the watchdog stayed armed
/// for the old, longer deadline - the exact drift the `--timeout` resync was
/// written to prevent, reopened via `--schedule` instead.
#[test]
fn editing_only_the_schedule_also_reclamps_an_agent_jobs_stored_time_budget() {
    let (_dir, store) = store();
    let mut job = spec("digest", "1h");
    job.kind = JobKind::Ai;
    job.command = "goal".to_string();
    job.agent = Some(AgentJobSpec {
        time_budget_secs: 900,
        token_budget: 50_000,
        ..Default::default()
    });
    store.create(&job, &env(), NOW, false).unwrap();

    let schedule = parse_schedule("30s").unwrap();
    store
        .patch(
            "digest",
            &CronJobPatch {
                schedule: Some((schedule, "30s".to_string())),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();

    let stored = store.get("digest").unwrap();
    assert_eq!(stored.timeout_secs, 30);
    let agent = stored.agent.expect("payload");
    assert_eq!(
        agent.time_budget_secs, 30,
        "the AI job's own stored time budget must shrink along with jobs.timeout_secs"
    );
    assert_eq!(
        agent.token_budget, 50_000,
        "the rest of the payload survives"
    );
}

#[test]
fn run_output_returns_the_full_stream_not_just_the_preview() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();

    let long_stdout = "line one\n".to_string() + &"x".repeat(200);
    let mut result = outcome(RunState::Succeeded);
    result.stdout = long_stdout.clone();
    result.stderr = "trouble".to_string();
    store
        .complete(&claimed[0].run_id, &result, NOW + 1)
        .unwrap();

    let output = store
        .run_output(&RunSelector::Latest("probe".to_string()))
        .unwrap();
    assert_eq!(output.stdout, long_stdout);
    assert_eq!(output.stderr, "trouble");
    assert_eq!(output.run.state, RunState::Succeeded);
    // `preview` (what `cron history` shows) is only the first line, capped at
    // 120 characters - `run_output` must not be limited the same way.
    assert_ne!(output.run.preview, long_stdout);
}

#[test]
fn run_output_by_id_finds_an_older_run_not_just_the_latest() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    make_due(&store, "probe", NOW);
    let first = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&first[0].run_id, NOW).unwrap();
    let mut first_outcome = outcome(RunState::Succeeded);
    first_outcome.stdout = "first\n".to_string();
    store
        .complete(&first[0].run_id, &first_outcome, NOW + 1)
        .unwrap();

    make_due(&store, "probe", NOW + 3600);
    let second = store
        .claim_due(NOW + 3600, "owner", 10, RunTrigger::Tick)
        .unwrap();
    store.start(&second[0].run_id, NOW + 3600).unwrap();
    let mut second_outcome = outcome(RunState::Succeeded);
    second_outcome.stdout = "second\n".to_string();
    store
        .complete(&second[0].run_id, &second_outcome, NOW + 3601)
        .unwrap();

    let latest = store
        .run_output(&RunSelector::Latest("probe".to_string()))
        .unwrap();
    assert_eq!(latest.stdout, "second\n");

    let older = store
        .run_output(&RunSelector::Id(first[0].run_id.clone()))
        .unwrap();
    assert_eq!(older.stdout, "first\n");
}

/// The bug this guards against: `RunSelector::Id` alone (unlike
/// `RunSelector::JobAndId`) never checks which job a run belongs to - so
/// `cron logs <job> --run <id>` used to silently return *any* job's run
/// matching `<id>`, discarding `<job>` entirely, if `<id>` did not happen to
/// belong to the named job.
#[test]
fn run_output_by_job_and_id_refuses_a_run_that_belongs_to_a_different_job() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    store
        .create(&spec("other", "1h"), &env(), NOW, false)
        .unwrap();

    make_due(&store, "probe", NOW);
    let probe_run = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&probe_run[0].run_id, NOW).unwrap();
    store
        .complete(&probe_run[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();

    make_due(&store, "other", NOW);
    let other_run = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&other_run[0].run_id, NOW).unwrap();
    store
        .complete(&other_run[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();

    // Asking for "other"'s own run under its own name still works.
    let matched = store
        .run_output(&RunSelector::JobAndId {
            job: "other".to_string(),
            run: other_run[0].run_id.clone(),
        })
        .unwrap();
    assert_eq!(matched.run.job_name, "other");

    // Asking for "probe"'s run while naming "other" must be refused, not
    // silently answered with probe's data under other's name.
    let error = store
        .run_output(&RunSelector::JobAndId {
            job: "other".to_string(),
            run: probe_run[0].run_id.clone(),
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("no run with this id under job"), "{error}");
}

#[test]
fn run_output_accepts_a_unique_prefix_and_refuses_an_ambiguous_one() {
    let (dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();

    make_due(&store, "probe", NOW);
    let first = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&first[0].run_id, NOW).unwrap();
    store
        .complete(&first[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();

    make_due(&store, "probe", NOW + 3600);
    let second = store
        .claim_due(NOW + 3600, "owner", 10, RunTrigger::Tick)
        .unwrap();
    store.start(&second[0].run_id, NOW + 3600).unwrap();
    store
        .complete(&second[0].run_id, &outcome(RunState::Succeeded), NOW + 3601)
        .unwrap();

    // Forced to share a prefix - only reachable in practice via an
    // astronomically unlikely UUID collision, so the test rewrites the ids
    // directly rather than depending on chance.
    let connection = raw(&dir);
    connection
        .execute(
            "UPDATE runs SET id = 'shared-aaa' WHERE id = ?1",
            [&first[0].run_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE runs SET id = 'shared-bbb' WHERE id = ?1",
            [&second[0].run_id],
        )
        .unwrap();

    let error = store
        .run_output(&RunSelector::Id("shared".to_string()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("more than one"), "{error}");

    let unique = store
        .run_output(&RunSelector::Id("shared-aaa".to_string()))
        .unwrap();
    assert_eq!(unique.run.id, "shared-aaa");
}

#[test]
fn run_output_clamps_at_the_stream_limit() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();
    let mut result = outcome(RunState::Succeeded);
    result.stdout = "x".repeat(20_000);
    store
        .complete(&claimed[0].run_id, &result, NOW + 1)
        .unwrap();

    let output = store
        .run_output(&RunSelector::Latest("probe".to_string()))
        .unwrap();
    assert!(output.stdout.len() < 20_000, "{}", output.stdout.len());
    assert!(output.stdout.contains("[truncated]"), "{}", output.stdout);
}

#[test]
fn run_output_on_a_job_with_no_finished_run_is_a_clear_error() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    let error = store
        .run_output(&RunSelector::Latest("probe".to_string()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no finished run"), "{error}");
}

#[test]
fn a_legacy_row_with_null_streams_reads_as_empty_not_an_error() {
    let (dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();
    store
        .complete(&claimed[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();

    // A row from before `stdout`/`stderr` existed, or one written by an
    // older `dsh` that never populated them.
    raw(&dir)
        .execute(
            "UPDATE runs SET stdout = NULL, stderr = NULL WHERE id = ?1",
            [&claimed[0].run_id],
        )
        .unwrap();

    let output = store
        .run_output(&RunSelector::Latest("probe".to_string()))
        .unwrap();
    assert_eq!(output.stdout, "");
    assert_eq!(output.stderr, "");
}

/// The bug this guards against: `complete_run`'s `agent_task_id = ?9` used to
/// overwrite whatever `attach_agent_task` had recorded at start with
/// whatever the outcome carried - `None` for every early-failure path
/// (config/budget/lock) that never got as far as starting the agent task -
/// erasing the one thing that would have made a lease-lost run findable.
#[test]
fn complete_does_not_clear_an_agent_task_id_recorded_at_start() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();
    store
        .attach_agent_task(&claimed[0].run_id, "task-123")
        .unwrap();

    let mut result = outcome(RunState::Failed);
    result.agent_task_id = None;
    store
        .complete(&claimed[0].run_id, &result, NOW + 1)
        .unwrap();

    let runs = store.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs[0].agent_task_id.as_deref(), Some("task-123"));
}

/// The other half of the watchdog-kill story: a run whose process group was
/// killed before it could ever call `complete` must still be traceable to
/// its agent task through `reap_expired_leases`.
#[test]
fn a_run_reaped_as_a_lost_lease_keeps_its_agent_task_id() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "5s"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store
        .claim_due(NOW, "dead-owner", 10, RunTrigger::Tick)
        .unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();
    store
        .attach_agent_task(&claimed[0].run_id, "task-456")
        .unwrap();

    let later = NOW + 121;
    store.reap_expired_leases(later).unwrap();

    let runs = store.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs[0].agent_task_id.as_deref(), Some("task-456"));
}

#[test]
fn attach_agent_task_is_visible_before_the_run_finishes() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();
    store
        .attach_agent_task(&claimed[0].run_id, "task-789")
        .unwrap();

    let runs = store
        .runs(&RunQuery {
            finished_only: false,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(runs[0].agent_task_id.as_deref(), Some("task-789"));
}

/// The bug this guards against: `RunSelector::Id`'s query used to build its
/// `LIKE` pattern from the caller-supplied prefix with no escaping, so a
/// literal `%` in it (not a real id prefix - just what someone typed) was
/// read as a wildcard matching every run in the store instead of failing
/// with "no run with this id".
#[test]
fn run_output_by_id_treats_wildcard_characters_in_the_prefix_literally() {
    let (_dir, store) = store();
    store
        .create(&spec("probe", "1h"), &env(), NOW, false)
        .unwrap();
    make_due(&store, "probe", NOW);
    let claimed = store.claim_due(NOW, "owner", 10, RunTrigger::Tick).unwrap();
    store.start(&claimed[0].run_id, NOW).unwrap();
    store
        .complete(&claimed[0].run_id, &outcome(RunState::Succeeded), NOW + 1)
        .unwrap();

    let error = store
        .run_output(&RunSelector::Id("%".to_string()))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no run with this id"),
        "a literal '%' must not match every run: {error}"
    );
}

/// The bug this guards against: `attach_agent_task`'s `UPDATE` never checked
/// its affected-row count, so a mismatched `run_id` would silently succeed
/// without recording anything - identical, from the caller's side, to a
/// real success, but quietly losing the one thing that makes a
/// watchdog-killed run findable again.
#[test]
fn attach_agent_task_errors_instead_of_silently_no_opping_on_an_unknown_run_id() {
    let (_dir, store) = store();
    assert!(store.attach_agent_task("no-such-run", "task-1").is_err());
}
