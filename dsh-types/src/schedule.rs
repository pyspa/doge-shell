//! Types shared between the `sched` builtin and the shell's scheduler.
//!
//! Scheduled tasks live for the duration of a shell session. Persistence is
//! deliberately left to `config.lisp`: `sched list --lisp` prints the calls that
//! recreate the current set.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{Duration, SystemTime};

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
    /// Renders back to the shortest exact spelling, so `sched list --lisp`
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
    /// Never say anything; check `sched log` or `out`.
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

/// Everything needed to register a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedTaskSpec {
    pub name: String,
    pub interval: IntervalSpec,
    pub command: String,
    pub cwd: String,
    pub notify: NotifyPolicy,
    pub timeout: Duration,
}

/// The outcome of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedRun {
    pub finished_at: SystemTime,
    pub duration_ms: u64,
    pub exit_code: i32,
    /// Output differed from the previous run.
    pub changed: bool,
    pub timed_out: bool,
    /// First line of output, for `sched list` / `sched log`.
    pub preview: String,
}

impl SchedRun {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// A read-only view of a task, as `sched list` prints it.
#[derive(Debug, Clone)]
pub struct SchedTaskView {
    pub id: u64,
    pub name: String,
    pub interval: IntervalSpec,
    pub command: String,
    pub cwd: String,
    pub notify: NotifyPolicy,
    pub paused: bool,
    /// Seconds until the next run, or `None` when paused.
    pub next_in: Option<u64>,
    pub running: bool,
    pub run_count: u64,
    pub fail_count: u64,
    pub last: Option<SchedRun>,
    pub history: Vec<SchedRun>,
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
}
