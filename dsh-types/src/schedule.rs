//! [`IntervalSpec`], [`NotifyPolicy`] and [`Schedule`]: the pieces of a cron
//! job's schedule that are not the cron-expression grammar itself (that is
//! `crate::cron`). Also home to `parse_schedule`, which tells the two grammars
//! apart. Persistence is `cron`'s own SQLite store
//! (`dsh/src/cron/store.rs`); nothing here is session-scoped any more - the
//! session-only `sched` builtin these types once served was replaced by
//! `cron`, which survives a restart.

use crate::cron::{CronExpr, parse_cron};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// Shortest interval accepted. Below this the shell spends more time spawning
/// than the task spends working, and the 1-second scan loop cannot honour it
/// accurately anyway.
pub const MIN_INTERVAL_SECS: u64 = 5;
/// Longest interval accepted. Anything rarer belongs in cron, which survives
/// logout.
pub const MAX_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Default per-run timeout, capped to the interval so a hung task cannot
/// overlap its own next run.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// A repeat interval, written `30s`, `5m` or `1h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalSpec {
    secs: u64,
}

impl IntervalSpec {
    pub fn secs(self) -> u64 {
        self.secs
    }

    pub fn duration(self) -> Duration {
        Duration::from_secs(self.secs)
    }
}

impl fmt::Display for IntervalSpec {
    /// Renders back to the shortest exact spelling, so a stored schedule
    /// round-trips through [`parse_interval`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secs.is_multiple_of(3600) {
            write!(f, "{}h", self.secs / 3600)
        } else if self.secs.is_multiple_of(60) {
            write!(f, "{}m", self.secs / 60)
        } else {
            write!(f, "{}s", self.secs)
        }
    }
}

/// Parses `30s` / `5m` / `1h`.
///
/// Only these three units are supported. Cron expressions are out of scope:
/// tasks do not outlive the session, so wall-clock scheduling would be
/// misleading.
pub fn parse_interval(spec: &str) -> Result<IntervalSpec, String> {
    let trimmed = spec.trim();
    let Some(unit) = trimmed.chars().last() else {
        return Err("empty interval".to_string());
    };
    let digits = &trimmed[..trimmed.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{spec}: expected a number followed by s, m or h"));
    }

    let value: u64 = digits
        .parse()
        .map_err(|_| format!("{spec}: number out of range"))?;

    let multiplier = match unit.to_ascii_lowercase() {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return Err(format!("{spec}: unknown unit '{unit}', expected s, m or h")),
    };

    let secs = value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("{spec}: interval out of range"))?;

    if secs < MIN_INTERVAL_SECS {
        return Err(format!("{spec}: minimum interval is {MIN_INTERVAL_SECS}s"));
    }
    if secs > MAX_INTERVAL_SECS {
        return Err(format!("{spec}: maximum interval is 24h"));
    }

    Ok(IntervalSpec { secs })
}

/// When a finished run should interrupt the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NotifyPolicy {
    /// Never say anything; check `cron history` instead.
    Never,
    /// Only when the command fails.
    OnFailure,
    /// Only when the output differs from the previous run.
    OnChange,
    /// Failure or changed output.
    #[default]
    Both,
    /// Every run.
    Always,
}

impl NotifyPolicy {
    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "never" | "quiet" => Self::Never,
            "failure" | "on-failure" => Self::OnFailure,
            "change" | "on-change" => Self::OnChange,
            "both" => Self::Both,
            "always" => Self::Always,
            _ => {
                return Err(format!(
                    "{name}: expected never, failure, change, both or always"
                ));
            }
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnFailure => "failure",
            Self::OnChange => "change",
            Self::Both => "both",
            Self::Always => "always",
        }
    }
}

impl fmt::Display for NotifyPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything the schedule position of `cron add` accepts.
///
/// `Every` keeps the `sched` grammar alive so an interval job reads the same
/// as it always did; the rest is wall-clock. The whole enum is `Copy` because
/// [`IntervalSpec`] and [`CronExpr`] both are, which lets a job spec stay cheap
/// to pass around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// `30s` / `5m` / `1h`, measured from the previous run.
    Every(IntervalSpec),
    /// A five-field expression or an `@` macro, on the local wall clock.
    Cron(CronExpr),
    /// `@reboot`. Fires once when an interactive session's runner starts; an
    /// external tick has no boot to speak of and ignores it.
    AtStartup,
    /// `@manual`. Only an explicit `cron run` fires it.
    Manual,
}

impl Schedule {
    /// The `schedule_kind` column's value. Stored alongside the spelling the
    /// user typed, so a row can be read back without re-parsing first.
    pub fn kind_str(self) -> &'static str {
        match self {
            Self::Every(_) => "every",
            Self::Cron(_) => "cron",
            Self::AtStartup => "startup",
            Self::Manual => "manual",
        }
    }

    /// Whether a due check against the wall clock can ever fire this.
    ///
    /// The two `false` arms are why the store keeps `next_run_at` NULL for
    /// them: a tick asks the database for due rows, so a startup-only or
    /// manual job is excluded by the query rather than by a special case.
    pub fn is_wall_clock(self) -> bool {
        matches!(self, Self::Every(_) | Self::Cron(_))
    }
}

impl fmt::Display for Schedule {
    /// Renders back to something [`parse_schedule`] accepts.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Every(interval) => interval.fmt(f),
            Self::Cron(expr) => expr.fmt(f),
            Self::AtStartup => f.write_str("@reboot"),
            Self::Manual => f.write_str("@manual"),
        }
    }
}

/// Characters that only ever appear in a cron expression, never in an
/// interval. Used to tell "you meant cron and forgot the quotes" apart from
/// "that is not an interval".
const CRON_MARKERS: [char; 4] = ['*', ',', '-', '/'];

/// Reads either grammar from one token.
///
/// The split is unambiguous: a cron expression either starts with `@` or has
/// five whitespace-separated fields, and an interval has neither.
pub fn parse_schedule(spec: &str) -> Result<Schedule, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("empty schedule".to_string());
    }
    if trimmed.eq_ignore_ascii_case("@reboot") {
        return Ok(Schedule::AtStartup);
    }
    if trimmed.eq_ignore_ascii_case("@manual") {
        return Ok(Schedule::Manual);
    }
    if trimmed.starts_with('@') || trimmed.contains(char::is_whitespace) {
        return parse_cron(trimmed).map(Schedule::Cron);
    }

    parse_interval(trimmed).map_err(|interval_error| {
        // A single token carrying `*` or `/` is almost always a five-field
        // expression the shell split apart before dsh saw it. Saying so beats
        // repeating the interval grammar at someone who never wanted it.
        if trimmed.contains(CRON_MARKERS) {
            format!(
                "{trimmed}: a cron expression needs 5 fields in one argument - quote it, as in '*/5 * * * *'"
            )
        } else {
            interval_error
        }
    })
    .map(Schedule::Every)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_unit() {
        assert_eq!(parse_interval("30s").unwrap().secs(), 30);
        assert_eq!(parse_interval("5m").unwrap().secs(), 300);
        assert_eq!(parse_interval("1h").unwrap().secs(), 3600);
        assert_eq!(parse_interval(" 2m ").unwrap().secs(), 120);
        assert_eq!(parse_interval("5M").unwrap().secs(), 300);
    }

    #[test]
    fn display_round_trips() {
        for spec in ["30s", "5m", "1h", "90s", "24h"] {
            let parsed = parse_interval(spec).unwrap();
            assert_eq!(parse_interval(&parsed.to_string()).unwrap(), parsed);
        }
        // 90s is not a whole number of minutes, so it stays in seconds.
        assert_eq!(parse_interval("90s").unwrap().to_string(), "90s");
        assert_eq!(parse_interval("120s").unwrap().to_string(), "2m");
        assert_eq!(parse_interval("60m").unwrap().to_string(), "1h");
    }

    #[test]
    fn rejects_bad_syntax() {
        assert!(parse_interval("").is_err());
        assert!(parse_interval("m").is_err());
        assert!(parse_interval("5").is_err());
        assert!(parse_interval("5d").is_err());
        assert!(parse_interval("-5m").is_err());
        assert!(parse_interval("5 m").is_err());
    }

    #[test]
    fn enforces_the_interval_bounds() {
        assert!(parse_interval("1s").is_err());
        assert_eq!(parse_interval("5s").unwrap().secs(), MIN_INTERVAL_SECS);
        assert_eq!(parse_interval("24h").unwrap().secs(), MAX_INTERVAL_SECS);
        assert!(parse_interval("25h").is_err());
    }

    #[test]
    fn does_not_overflow_on_huge_numbers() {
        assert!(parse_interval("99999999999999999999h").is_err());
        assert!(parse_interval("18446744073709551615h").is_err());
    }

    #[test]
    fn notify_policy_round_trips() {
        for policy in [
            NotifyPolicy::Never,
            NotifyPolicy::OnFailure,
            NotifyPolicy::OnChange,
            NotifyPolicy::Both,
            NotifyPolicy::Always,
        ] {
            assert_eq!(NotifyPolicy::parse(policy.as_str()), Ok(policy));
        }
        assert_eq!(
            NotifyPolicy::parse("on-failure"),
            Ok(NotifyPolicy::OnFailure)
        );
        assert_eq!(NotifyPolicy::parse("quiet"), Ok(NotifyPolicy::Never));
        assert!(NotifyPolicy::parse("sometimes").is_err());
        assert_eq!(NotifyPolicy::default(), NotifyPolicy::Both);
    }

    #[test]
    fn parse_schedule_reads_both_grammars() {
        assert_eq!(
            parse_schedule("5m"),
            Ok(Schedule::Every(parse_interval("5m").unwrap()))
        );
        assert_eq!(
            parse_schedule("*/5 * * * *"),
            Ok(Schedule::Cron(parse_cron("*/5 * * * *").unwrap()))
        );
        assert_eq!(
            parse_schedule("@daily"),
            Ok(Schedule::Cron(parse_cron("@daily").unwrap()))
        );
        assert_eq!(parse_schedule("@reboot"), Ok(Schedule::AtStartup));
        assert_eq!(parse_schedule("@REBOOT"), Ok(Schedule::AtStartup));
        assert_eq!(parse_schedule("@manual"), Ok(Schedule::Manual));
        assert_eq!(parse_schedule("  1h  "), parse_schedule("1h"));
    }

    #[test]
    fn schedule_display_round_trips() {
        for spec in ["30s", "5m", "1h", "@reboot", "@manual"] {
            let parsed = parse_schedule(spec).unwrap();
            assert_eq!(parse_schedule(&parsed.to_string()), Ok(parsed), "{spec}");
        }
        // A cron expression normalises rather than echoing, but must still
        // parse back to the same schedule.
        for spec in ["*/5 * * * *", "@daily", "0 9-17 * * mon-fri"] {
            let parsed = parse_schedule(spec).unwrap();
            assert_eq!(parse_schedule(&parsed.to_string()), Ok(parsed), "{spec}");
        }
    }

    /// An unquoted five-field expression reaches dsh as `*` after the shell
    /// has globbed it. The interval grammar is the wrong thing to explain.
    #[test]
    fn an_unquoted_cron_expression_says_to_quote_it() {
        for fragment in ["*", "*/5", "1,15", "9-17"] {
            let error = parse_schedule(fragment).unwrap_err();
            assert!(error.contains("quote it"), "{fragment}: {error}");
        }
        // A plain typo still gets the interval message.
        let error = parse_schedule("5x").unwrap_err();
        assert!(error.contains("expected s, m or h"), "{error}");
    }

    #[test]
    fn schedule_kind_matches_the_stored_column() {
        assert_eq!(parse_schedule("5m").unwrap().kind_str(), "every");
        assert_eq!(parse_schedule("@daily").unwrap().kind_str(), "cron");
        assert_eq!(parse_schedule("@reboot").unwrap().kind_str(), "startup");
        assert_eq!(parse_schedule("@manual").unwrap().kind_str(), "manual");
    }

    /// The due query only ever sees wall-clock schedules; the other two are
    /// excluded by a NULL `next_run_at` rather than by a branch in the tick.
    #[test]
    fn only_wall_clock_schedules_are_due_checked() {
        assert!(parse_schedule("5m").unwrap().is_wall_clock());
        assert!(parse_schedule("@daily").unwrap().is_wall_clock());
        assert!(!parse_schedule("@reboot").unwrap().is_wall_clock());
        assert!(!parse_schedule("@manual").unwrap().is_wall_clock());
    }
}
