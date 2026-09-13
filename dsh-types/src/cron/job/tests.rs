use super::*;

/// Every enum in this module is stored as text in SQLite and printed in
/// `--json`. A variant whose `as_str` and `parse` disagree does not fail to
/// compile - it fails at read time, on a row written by an earlier run.
///
/// The `ALL` lists below are kept honest by `exhaustive_*` guards: those match
/// on every variant, so adding one without listing it stops the build.
fn assert_round_trips<T>(
    all: &[T],
    parse: fn(&str) -> Result<T, String>,
    as_str: fn(T) -> &'static str,
) where
    T: Copy + PartialEq + std::fmt::Debug,
{
    for value in all {
        let text = as_str(*value);
        assert_eq!(parse(text), Ok(*value), "{text}");
    }
    let spellings: Vec<&str> = all.iter().map(|value| as_str(*value)).collect();
    let mut unique = spellings.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        spellings.len(),
        "two variants share a spelling"
    );
    assert!(parse("nonsense-value").is_err());
}

const ALL_KINDS: [JobKind; 2] = [JobKind::Sh, JobKind::Ai];
const ALL_STATES: [RunState; 7] = [
    RunState::Queued,
    RunState::Running,
    RunState::Succeeded,
    RunState::Failed,
    RunState::Skipped,
    RunState::NeedsApproval,
    RunState::Cancelled,
];
const ALL_REASONS: [RunReason; 12] = [
    RunReason::AgentBusy,
    RunReason::StillRunning,
    RunReason::BudgetExhausted,
    RunReason::Transient,
    RunReason::Timeout,
    RunReason::Config,
    RunReason::Provider,
    RunReason::Spawn,
    RunReason::RootChanged,
    RunReason::HookDeny,
    RunReason::StateUnusable,
    RunReason::Reconcile,
];
const ALL_TRIGGERS: [RunTrigger; 4] = [
    RunTrigger::Session,
    RunTrigger::Tick,
    RunTrigger::Manual,
    RunTrigger::Startup,
];
const ALL_INCIDENTS: [IncidentKind; 11] = [
    IncidentKind::Approval,
    IncidentKind::HookAsk,
    IncidentKind::Reconcile,
    IncidentKind::Budget,
    IncidentKind::Provider,
    IncidentKind::Config,
    IncidentKind::Failing,
    IncidentKind::LockStarvation,
    IncidentKind::LeaseLost,
    IncidentKind::StateUnusable,
    IncidentKind::RootChanged,
];

#[test]
fn every_enum_round_trips_through_its_stored_spelling() {
    assert_round_trips(&ALL_KINDS, JobKind::parse, JobKind::as_str);
    assert_round_trips(&ALL_STATES, RunState::parse, RunState::as_str);
    assert_round_trips(&ALL_REASONS, RunReason::parse, RunReason::as_str);
    assert_round_trips(&ALL_TRIGGERS, RunTrigger::parse, RunTrigger::as_str);
    assert_round_trips(&ALL_INCIDENTS, IncidentKind::parse, IncidentKind::as_str);
}

#[test]
fn display_agrees_with_as_str() {
    assert_eq!(JobKind::Ai.to_string(), "ai");
    assert_eq!(RunState::NeedsApproval.to_string(), "needs-approval");
    assert_eq!(RunReason::AgentBusy.to_string(), "agent-busy");
    assert_eq!(RunTrigger::Tick.to_string(), "tick");
    assert_eq!(IncidentKind::LockStarvation.to_string(), "lock-starvation");
}

/// Adding a variant without extending the `ALL_*` list above would otherwise
/// leave it untested. These match arms are the tripwire.
#[test]
fn exhaustive_variant_lists() {
    fn kind(value: JobKind) -> usize {
        match value {
            JobKind::Sh | JobKind::Ai => ALL_KINDS.len(),
        }
    }
    fn state(value: RunState) -> usize {
        match value {
            RunState::Queued
            | RunState::Running
            | RunState::Succeeded
            | RunState::Failed
            | RunState::Skipped
            | RunState::NeedsApproval
            | RunState::Cancelled => ALL_STATES.len(),
        }
    }
    fn reason(value: RunReason) -> usize {
        match value {
            RunReason::AgentBusy
            | RunReason::StillRunning
            | RunReason::BudgetExhausted
            | RunReason::Transient
            | RunReason::Timeout
            | RunReason::Config
            | RunReason::Provider
            | RunReason::Spawn
            | RunReason::RootChanged
            | RunReason::HookDeny
            | RunReason::StateUnusable
            | RunReason::Reconcile => ALL_REASONS.len(),
        }
    }
    fn trigger(value: RunTrigger) -> usize {
        match value {
            RunTrigger::Session | RunTrigger::Tick | RunTrigger::Manual | RunTrigger::Startup => {
                ALL_TRIGGERS.len()
            }
        }
    }
    fn incident(value: IncidentKind) -> usize {
        match value {
            IncidentKind::Approval
            | IncidentKind::HookAsk
            | IncidentKind::Reconcile
            | IncidentKind::Budget
            | IncidentKind::Provider
            | IncidentKind::Config
            | IncidentKind::Failing
            | IncidentKind::LockStarvation
            | IncidentKind::LeaseLost
            | IncidentKind::StateUnusable
            | IncidentKind::RootChanged => ALL_INCIDENTS.len(),
        }
    }
    assert_eq!(kind(JobKind::Sh), 2);
    assert_eq!(state(RunState::Queued), 7);
    assert_eq!(reason(RunReason::Timeout), 12);
    assert_eq!(trigger(RunTrigger::Tick), 4);
    assert_eq!(incident(IncidentKind::Approval), 11);
}

/// `history` shows finished runs, `runs` shows the rest. A run recorded at
/// claim time is not finished, which is what lets a crash be noticed.
#[test]
fn only_terminal_states_are_finished() {
    for state in ALL_STATES {
        let finished = !matches!(state, RunState::Queued | RunState::Running);
        assert_eq!(state.is_finished(), finished, "{state}");
    }
}

/// A skipped run must not move the failure streak: "another agent task was
/// running" is not this job failing, and `--on failure` must stay quiet.
#[test]
fn only_failure_counts_as_failure() {
    for state in ALL_STATES {
        assert_eq!(
            state.counts_as_failure(),
            state == RunState::Failed,
            "{state}"
        );
    }
    assert!(!RunState::Skipped.counts_as_failure());
    assert!(!RunState::NeedsApproval.counts_as_failure());
    assert!(!RunState::Cancelled.counts_as_failure());
}

/// Retrying something that will answer identically burns tokens and buries the
/// incident that explains it.
#[test]
fn a_settled_reason_is_not_retried() {
    for reason in [
        RunReason::AgentBusy,
        RunReason::StillRunning,
        RunReason::Transient,
        RunReason::Timeout,
    ] {
        assert!(reason.is_worth_retrying(), "{reason}");
    }
    for reason in [
        RunReason::Config,
        RunReason::Provider,
        RunReason::BudgetExhausted,
        RunReason::RootChanged,
        RunReason::HookDeny,
        RunReason::StateUnusable,
        RunReason::Reconcile,
        RunReason::Spawn,
    ] {
        assert!(!reason.is_worth_retrying(), "{reason}");
    }
}

/// A failing streak keeps trying - whatever it waits on may come back. The
/// rest need a person, so the job stops until it is acknowledged.
///
/// `StateUnusable` belongs in the blocking group despite living in the same
/// "not an ordinary failure" family as `Budget`/`LockStarvation`/`LeaseLost`:
/// its own doc comment ("every agent job is stopped") describes a store or
/// state directory that is broken, not merely busy or waiting on a budget -
/// nothing about the next tick can fix that, so retrying is not a kindness.
#[test]
fn only_unresolvable_incidents_block_the_job() {
    for kind in [
        IncidentKind::Approval,
        IncidentKind::HookAsk,
        IncidentKind::Reconcile,
        IncidentKind::Provider,
        IncidentKind::Config,
        IncidentKind::RootChanged,
        IncidentKind::StateUnusable,
    ] {
        assert!(kind.blocks_the_job(), "{kind}");
    }
    for kind in [
        IncidentKind::Failing,
        IncidentKind::Budget,
        IncidentKind::LockStarvation,
        IncidentKind::LeaseLost,
    ] {
        assert!(!kind.blocks_the_job(), "{kind}");
    }
}

fn view() -> CronJobView {
    CronJobView {
        id: 1,
        name: "probe".to_string(),
        kind: JobKind::Sh,
        schedule: crate::schedule::parse_schedule("5m").unwrap(),
        schedule_spec: "5m".to_string(),
        command: "true".to_string(),
        agent: None,
        cwd: "/tmp".to_string(),
        notify: NotifyPolicy::default(),
        timeout_secs: 60,
        catchup_secs: 3600,
        paused: false,
        blocked: false,
        next_run_at: Some(0),
        running: false,
        run_count: 0,
        fail_count: 0,
        consecutive_failures: 0,
        last: None,
    }
}

/// Blocked outranks paused: a job held back by an incident that a person must
/// clear should not read as something they chose to pause.
#[test]
fn state_label_reports_the_most_urgent_condition() {
    assert_eq!(view().state_label(), "ok");

    let mut failing = view();
    failing.consecutive_failures = 2;
    assert_eq!(failing.state_label(), "failing");

    let mut running = failing.clone();
    running.running = true;
    assert_eq!(running.state_label(), "running");

    let mut paused = running.clone();
    paused.paused = true;
    assert_eq!(paused.state_label(), "paused");

    let mut blocked = paused.clone();
    blocked.blocked = true;
    assert_eq!(blocked.state_label(), "blocked");
}

/// The payload column is JSON, so this shape has to survive a round trip
/// through `serde_json` unchanged - including an absent daily ceiling, which
/// is not the same as a ceiling of zero.
#[test]
fn agent_spec_round_trips_through_json() {
    let spec = AgentJobSpec {
        grant: TaskGrant {
            read_roots: vec!["/tmp".into()],
            write_roots: vec!["/tmp/out".into()],
            commands: vec!["cargo test -p dsh-types".to_string()],
            mcp_calls: vec![],
            network_hosts: vec!["example.com".to_string()],
            environment: vec!["HOME".to_string()],
            sandbox: true,
        },
        criteria: vec!["the report exists".to_string()],
        token_budget: 50_000,
        time_budget_secs: 900,
        max_tokens_per_day: None,
    };
    let json = serde_json::to_string(&spec).unwrap();
    assert_eq!(serde_json::from_str::<AgentJobSpec>(&json).unwrap(), spec);

    let capped = AgentJobSpec {
        max_tokens_per_day: Some(200_000),
        ..spec
    };
    let json = serde_json::to_string(&capped).unwrap();
    assert_eq!(serde_json::from_str::<AgentJobSpec>(&json).unwrap(), capped);
}

/// `cron edit` with no field flags must be told apart from one that clears a
/// field, or an accidental bare `cron edit NAME` would wipe the job.
#[test]
fn an_untouched_patch_is_empty() {
    assert!(CronJobPatch::default().is_empty());
    assert!(
        !CronJobPatch {
            command: Some(String::new()),
            ..Default::default()
        }
        .is_empty()
    );
}
