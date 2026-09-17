use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn a_bare_shell_command_parses_with_defaults() {
    let parsed = parse_add(&args(&["5m", "echo", "hi"])).unwrap();
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
fn editing_nothing_is_refused() {
    assert!(parse_edit(&args(&["probe"])).is_err());
}

#[test]
fn editing_one_field_leaves_other_fields_alone() {
    let (name, patch) = parse_edit(&args(&["probe", "--on", "failure"])).unwrap();
    assert_eq!(name, "probe");
    assert_eq!(patch.notify, Some(NotifyPolicy::OnFailure));
    assert!(patch.agent.is_none());
    assert!(patch.command.is_none());
}

#[test]
fn quiet_is_recognised_in_edit_without_consuming_the_next_token() {
    let (name, patch) = parse_edit(&args(&["probe", "--quiet", "--cwd", "/x"])).unwrap();
    assert_eq!(name, "probe");
    assert_eq!(patch.notify, Some(NotifyPolicy::Never));
    assert_eq!(patch.cwd.as_deref(), Some("/x"));
}

/// `--timeout` is valid on a plain shell job and must not be refused just
/// because there is no existing agent spec to resync.
#[test]
fn editing_only_timeout_on_a_shell_job_does_not_need_an_agent_spec() {
    let (_, patch) = parse_edit(&args(&["probe", "--timeout", "5m"])).unwrap();
    assert_eq!(patch.timeout_secs, Some(300));
    assert!(patch.agent.is_none());
}
