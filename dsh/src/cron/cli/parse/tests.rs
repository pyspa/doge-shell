use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn a_bare_shell_command_parses_with_defaults() {
    let parsed = parse_add(&args(&["5m", "echo", "hi"])).unwrap();
    assert!(!parsed.agent);
    assert_eq!(parsed.schedule_spec, "5m");
    assert_eq!(parsed.command, vec!["echo", "hi"]);
    assert_eq!(parsed.notify, NotifyPolicy::default());
    assert_eq!(parsed.catchup_secs, DEFAULT_CATCHUP_SECS);
}

#[test]
fn options_before_the_schedule_are_all_recognised() {
    let parsed = parse_add(&args(&[
        "--name",
        "probe",
        "--on",
        "failure",
        "--timeout",
        "90s",
        "--catchup",
        "10m",
        "--paused",
        "5m",
        "echo",
        "hi",
    ]))
    .unwrap();
    assert_eq!(parsed.name.as_deref(), Some("probe"));
    assert_eq!(parsed.notify, NotifyPolicy::OnFailure);
    assert_eq!(parsed.timeout_secs, Some(90));
    assert_eq!(parsed.catchup_secs, 600);
    assert!(parsed.paused);
}

#[test]
fn quiet_is_shorthand_for_on_never() {
    let parsed = parse_add(&args(&["--quiet", "5m", "true"])).unwrap();
    assert_eq!(parsed.notify, NotifyPolicy::Never);
}

/// A `--timeout` in bare seconds must work exactly like the interval form -
/// `agent run --timeout` already takes bare seconds, and cron's grammar has
/// to match it.
#[test]
fn timeout_accepts_bare_seconds_or_an_interval() {
    assert_eq!(
        parse_add(&args(&["--timeout", "45", "5m", "x"]))
            .unwrap()
            .timeout_secs,
        Some(45)
    );
    assert_eq!(
        parse_add(&args(&["--timeout", "2m", "5m", "x"]))
            .unwrap()
            .timeout_secs,
        Some(120)
    );
}

/// A bare-seconds `--timeout` past `i64::MAX` must be rejected here, not
/// allowed to reach `store/claim.rs::lease_secs`'s `as i64` cast, where an
/// overflow would silently produce an artificially *short* lease for a job
/// that asked for a huge timeout - backwards from what the value means, and
/// enough to let a second process start the same job while the first one is
/// still (legitimately) running.
#[test]
fn an_overflowing_bare_timeout_is_rejected() {
    let huge = format!("{}", u64::MAX);
    let error = parse_add(&args(&["--timeout", &huge, "5m", "x"])).unwrap_err();
    assert!(error.contains("too large"), "{error}");
}

/// The bug this guards against: the old bound only rejected values *past*
/// `i64::MAX`, but `lease_secs` doubles whatever it is given - a value merely
/// under `i64::MAX` (this one included) still overflows once doubled, and
/// `store/claim.rs` adding that to `now` would either panic (debug) or wrap
/// to a bogus, already-"expired" `claimed_until` (release).
#[test]
fn a_bare_timeout_that_would_overflow_once_doubled_is_rejected() {
    let error = parse_add(&args(&["--timeout", "9223372036854775807", "5m", "x"])).unwrap_err();
    assert!(error.contains("too large"), "{error}");
}

#[test]
fn an_unquoted_cron_expression_gets_a_fixable_error() {
    for scattered in [["*/5", "*", "*", "*", "*"], ["*", "git", "fetch", "x", "y"]] {
        let error = parse_add(&args(&scattered)).unwrap_err();
        assert!(error.contains("quote"), "{scattered:?}: {error}");
    }
}

/// A `--` before a shell command must not become part of the command line
/// that later reaches `sh -c` — `sh -c '-- exit 3'` fails with "invalid
/// option", not the exit code the caller wanted.
#[test]
fn a_leading_separator_is_stripped_from_a_shell_command_too() {
    let parsed = parse_add(&args(&["--quiet", "5m", "--", "exit", "3"])).unwrap();
    assert_eq!(parsed.command, vec!["exit", "3"]);
}

#[test]
fn an_agent_job_needs_the_separator_before_its_goal() {
    let parsed = parse_add(&args(&[
        "--agent",
        "--tokens",
        "1000",
        "--write",
        "/tmp",
        "@daily",
        "--",
        "summarise",
        "today",
    ]))
    .unwrap();
    assert!(parsed.agent);
    assert_eq!(parsed.command, vec!["summarise", "today"]);
    assert_eq!(parsed.token_budget, 1000);
    assert_eq!(
        parsed.grant.write_roots,
        vec![std::path::PathBuf::from("/tmp")]
    );
}

#[test]
fn grant_options_reuse_the_shared_agent_run_validation() {
    let error = parse_add(&args(&[
        "--agent",
        "--tokens",
        "10",
        "--network",
        "10.0.0.0/8",
        "@daily",
        "--",
        "x",
    ]))
    .unwrap_err();
    assert!(error.contains("exact host"), "{error}");
}

#[test]
fn a_missing_command_is_a_clear_error() {
    assert!(parse_add(&args(&["5m"])).is_err());
    assert!(parse_add(&args(&["--agent", "--tokens", "1", "@daily"])).is_err());
}

#[test]
fn default_names_differ_by_kind() {
    assert_eq!(
        default_job_name(false, &["git".into(), "fetch".into()]),
        "git"
    );
    assert_eq!(
        default_job_name(
            true,
            &[
                "Summarise".into(),
                "the".into(),
                "PRs!".into(),
                "today".into()
            ]
        ),
        "summarise-the-prs"
    );
}

fn spec(args_slice: &[&str]) -> CronJobSpec {
    build_spec(parse_add(&args(args_slice)).unwrap(), "/cwd".to_string()).unwrap()
}

#[test]
fn build_spec_defaults_the_name_and_cwd() {
    let s = spec(&["5m", "echo", "hi"]);
    assert_eq!(s.name, "echo");
    assert_eq!(s.cwd, "/cwd");
    assert_eq!(s.kind, JobKind::Sh);
    assert!(s.agent.is_none());
}

/// An interval job must not be able to outlive its own interval - the next
/// run would never get a turn.
#[test]
fn an_interval_jobs_timeout_is_capped_to_the_interval() {
    let s = spec(&["--timeout", "10m", "5m", "echo", "hi"]);
    assert_eq!(s.timeout_secs, 300);
}

/// A cron expression has no fixed interval to compare against, so its
/// timeout is not silently reduced.
#[test]
fn a_cron_jobs_timeout_is_not_capped() {
    let s = spec(&["--timeout", "10m", "@daily", "echo", "hi"]);
    assert_eq!(s.timeout_secs, 600);
}

#[test]
fn an_agent_job_needs_at_least_one_grant_root() {
    let error = build_spec(
        parse_add(&args(&[
            "--agent", "--tokens", "10", "@daily", "--", "goal",
        ]))
        .unwrap(),
        "/cwd".to_string(),
    )
    .unwrap_err();
    assert!(error.contains("--read or --write"), "{error}");
}

#[test]
fn an_agent_jobs_timeout_drives_both_the_lease_and_the_task_budget() {
    let s = spec(&[
        "--agent",
        "--tokens",
        "10",
        "--timeout",
        "5m",
        "--write",
        "/tmp",
        "@daily",
        "--",
        "goal",
    ]);
    assert_eq!(s.timeout_secs, 300);
    assert_eq!(s.agent.as_ref().unwrap().time_budget_secs, 300);
}

#[test]
fn editing_nothing_is_refused() {
    assert!(parse_edit(&args(&["probe"]), None).is_err());
}

#[test]
fn editing_one_field_leaves_the_agent_payload_alone() {
    let (name, patch) = parse_edit(&args(&["probe", "--on", "failure"]), None).unwrap();
    assert_eq!(name, "probe");
    assert_eq!(patch.notify, Some(NotifyPolicy::OnFailure));
    assert!(patch.agent.is_none());
    assert!(patch.command.is_none());
}

#[test]
fn editing_a_grant_field_builds_a_full_agent_payload() {
    let existing = AgentJobSpec::default();
    let (_, patch) = parse_edit(
        &args(&["probe", "--allow-command", "cargo test"]),
        Some(&existing),
    )
    .unwrap();
    let agent = patch.agent.unwrap();
    assert_eq!(agent.grant.commands, vec!["cargo test"]);
}

/// The bug this guards against: a grant-shaped flag on a job with no
/// existing agent spec used to silently fabricate one (`token_budget: 0`, an
/// empty grant) - `job.kind` stayed `sh`, but `cron show`/`doctor` started
/// rendering a bogus "agent:" section for what is really a shell job.
#[test]
fn a_grant_field_on_a_job_with_no_agent_spec_is_refused() {
    let error = parse_edit(&args(&["probe", "--allow-command", "cargo test"]), None).unwrap_err();
    assert!(error.contains("not an agent job"), "{error}");
}

/// The bug this guards against: editing one grant flag on a job that already
/// has other grants, criteria, and a token budget must not wipe them - the
/// store writes the whole `AgentJobSpec` back (`store/api.rs::patch`), so
/// `parse_edit` is the only place that can merge onto what already exists.
#[test]
fn editing_a_grant_field_on_an_existing_agent_job_preserves_the_rest() {
    let existing = AgentJobSpec {
        grant: TaskGrant {
            read_roots: vec!["/data".into()],
            commands: vec!["cargo test".into()],
            ..TaskGrant::default()
        },
        criteria: vec!["tests pass".into()],
        token_budget: 50_000,
        time_budget_secs: 900,
        max_tokens_per_day: Some(200_000),
    };
    let (_, patch) = parse_edit(
        &args(&["probe", "--check", "docs updated"]),
        Some(&existing),
    )
    .unwrap();
    let agent = patch.agent.unwrap();
    assert_eq!(
        agent.grant.read_roots,
        vec![std::path::PathBuf::from("/data")]
    );
    assert_eq!(agent.grant.commands, vec!["cargo test"]);
    assert_eq!(agent.criteria, vec!["tests pass", "docs updated"]);
    assert_eq!(agent.token_budget, 50_000);
    assert_eq!(agent.time_budget_secs, 900);
    assert_eq!(agent.max_tokens_per_day, Some(200_000));
}

#[test]
fn quiet_is_recognised_in_edit_without_consuming_the_next_token() {
    let (name, patch) = parse_edit(&args(&["probe", "--quiet", "--cwd", "/x"]), None).unwrap();
    assert_eq!(name, "probe");
    assert_eq!(patch.notify, Some(NotifyPolicy::Never));
    assert_eq!(patch.cwd.as_deref(), Some("/x"));
}

/// `--timeout` is valid on a plain shell job and must not be refused just
/// because there is no existing agent spec to resync.
#[test]
fn editing_only_timeout_on_a_shell_job_does_not_need_an_agent_spec() {
    let (_, patch) = parse_edit(&args(&["probe", "--timeout", "5m"]), None).unwrap();
    assert_eq!(patch.timeout_secs, Some(300));
    assert!(patch.agent.is_none());
}

/// The bug this guards against: `--timeout` alone used to leave the stored
/// `AgentJobSpec.time_budget_secs` stale, desyncing it from the fresh
/// `jobs.timeout_secs` the claim lease is computed from - see `run_job.rs`'s
/// and `store/claim.rs`'s "kept in lock-step" comments.
#[test]
fn editing_only_timeout_on_an_agent_job_resyncs_the_time_budget() {
    let existing = AgentJobSpec {
        grant: TaskGrant {
            read_roots: vec!["/data".into()],
            ..TaskGrant::default()
        },
        criteria: vec!["tests pass".into()],
        token_budget: 50_000,
        time_budget_secs: 60,
        max_tokens_per_day: None,
    };
    let (_, patch) = parse_edit(&args(&["probe", "--timeout", "10m"]), Some(&existing)).unwrap();
    assert_eq!(patch.timeout_secs, Some(600));
    let agent = patch.agent.unwrap();
    assert_eq!(agent.time_budget_secs, 600, "must track the new timeout");
    assert_eq!(
        agent.grant.read_roots,
        vec![std::path::PathBuf::from("/data")]
    );
    assert_eq!(agent.criteria, vec!["tests pass"]);
    assert_eq!(agent.token_budget, 50_000);
}
