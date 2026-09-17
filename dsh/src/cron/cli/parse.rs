//! Turning `cron` argv into store-ready specs.

use anyhow::{Context as _, Result};
use dsh_types::cron::job::{CronJobPatch, CronJobSpec, JobKind};
use dsh_types::schedule::{DEFAULT_TIMEOUT_SECS, NotifyPolicy, Schedule, parse_schedule};

/// Default lease/lookback window for a missed schedule slot.
pub const DEFAULT_CATCHUP_SECS: u64 = 3600;
/// Longest a job name may be, and the character set it is drawn from.
///
/// Enforced here, not just slugified on the way in: the name becomes a
/// filename (the notepad) and a lease-file name, so it must be safe as one
/// without a second layer of escaping.
pub(crate) const MAX_NAME_LEN: usize = 64;

/// A schedule token that looks like an unquoted cron expression the shell
/// already tore apart — `*/5`, `1,15`, `9-17` land as this argument on their
/// own once the rest of the line got word-split.
fn looks_like_a_scattered_cron_field(token: &str) -> bool {
    token == "*"
        || token.contains(['*', ',', '-', '/'])
            && token.chars().any(|c| c.is_ascii_digit() || c == '*')
}

/// Parses the schedule argument, turning the shell's own doing into a
/// specific, fixable error rather than a confusing interval-grammar message.
pub fn parse_schedule_arg(token: &str) -> Result<Schedule, String> {
    parse_schedule(token).map_err(|error| {
        if looks_like_a_scattered_cron_field(token) {
            format!(
                "cron add: quote the schedule: cron add '<the whole expression>' <command...> \
                 (got `{token}`, which looks like a piece of one that the shell already split)"
            )
        } else {
            error
        }
    })
}

/// Everything `cron add` parsed, before it is turned into a [`CronJobSpec`].
#[derive(Debug, Default)]
pub struct AddArgs {
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub notify: NotifyPolicy,
    pub timeout_secs: Option<u64>,
    pub catchup_secs: u64,
    pub paused: bool,
    pub force: bool,
    pub schedule_spec: String,
    pub schedule: Option<Schedule>,
    /// The command line for a shell job.
    pub command: Vec<String>,
}

fn parse_named_duration(value: &str) -> Result<u64, String> {
    if let Ok(secs) = value.parse::<u64>() {
        // The bare-seconds spelling has no upper bound of its own the way the
        // suffixed form does (`parse_interval`'s `MAX_INTERVAL_SECS`); without
        // one, a value like `u64::MAX` overflows `i64` further down the line
        // (`store/claim.rs::lease_secs` casts this to `i64` and *doubles* it),
        // silently producing a *small* lease for a job that claimed a *huge*
        // timeout - exactly backwards, and enough to let a second process
        // start the same job again while the first is still running.
        //
        // The bound is `i64::MAX / 2`, not `i64::MAX`: `lease_secs` doubles
        // whatever fits in an `i64`, so a value merely under `i64::MAX` still
        // overflows once doubled (`lease_secs`'s own `saturating_mul` catches
        // that, but the claim that adds the result to `now` cannot un-overflow
        // what already saturated - see `store/claim.rs`).
        if secs > i64::MAX as u64 / 2 {
            return Err(format!(
                "{value}: too large; use a smaller number of seconds"
            ));
        }
        return Ok(secs);
    }
    dsh_types::schedule::parse_interval(value).map(|interval| interval.secs())
}

/// Parses `cron add`'s options, then the schedule, then the command.
pub fn parse_add(args: &[String]) -> Result<AddArgs, String> {
    let mut out = AddArgs {
        catchup_secs: DEFAULT_CATCHUP_SECS,
        ..Default::default()
    };
    let mut index = 0;

    while index < args.len() {
        let option = args[index].as_str();
        if option == "--" {
            index += 1;
            break;
        }
        if option == "--quiet" {
            out.notify = NotifyPolicy::Never;
            index += 1;
            continue;
        }
        if option == "--paused" {
            out.paused = true;
            index += 1;
            continue;
        }
        if option == "--force" {
            out.force = true;
            index += 1;
            continue;
        }
        if !option.starts_with("--") {
            // First non-option token: the schedule.
            break;
        }
        index += 1;
        let value = args
            .get(index)
            .ok_or(format!("{option} requires a value"))?;
        index += 1;

        match option {
            "--name" => {
                if value.len() > MAX_NAME_LEN || value.is_empty() {
                    return Err(format!("--name must be 1-{MAX_NAME_LEN} characters"));
                }
                out.name = Some(value.clone());
            }
            "--cwd" => out.cwd = Some(value.clone()),
            "--on" => out.notify = NotifyPolicy::parse(value)?,
            "--timeout" => out.timeout_secs = Some(parse_named_duration(value)?),
            "--catchup" => out.catchup_secs = parse_named_duration(value)?,
            _ => return Err(format!("{option}: unknown option")),
        }
    }

    let schedule_spec = args
        .get(index)
        .ok_or("expected a schedule (e.g. '5m' or '0 9 * * *')")?;
    out.schedule = Some(parse_schedule_arg(schedule_spec)?);
    out.schedule_spec = schedule_spec.clone();
    index += 1;

    // An optional `--` separates options from the command, rather than silently
    // folding a stray `--` into the command line it runs (`sh -c '-- exit 3'`
    // is not `exit 3`; it is `sh` complaining about `--` as an option).
    let rest = &args[index..];
    let command = match rest.first().map(String::as_str) {
        Some("--") => rest[1..].to_vec(),
        _ => rest.to_vec(),
    };
    if command.is_empty() {
        return Err("expected a command to run".to_string());
    }
    out.command = command;
    Ok(out)
}

/// The name a job gets when `--name` was not given.
///
/// A shell job takes its first word, matching `sched`'s old default.
pub fn default_job_name(command: &[String]) -> String {
    command
        .join(" ")
        .split_whitespace()
        .next()
        .unwrap_or("job")
        .to_string()
}

/// Builds the spec `cron add` hands to the store, given the parsed options
/// and the current directory to default `--cwd` to.
pub fn build_spec(args: AddArgs, default_cwd: String) -> Result<CronJobSpec, String> {
    let name = args.name.unwrap_or_else(|| default_job_name(&args.command));

    let schedule = args
        .schedule
        .expect("schedule is always parsed by parse_add");
    let mut timeout_secs = args.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    // An interval job that outlives its own interval would starve its own
    // next run; a cron expression has no fixed interval to compare against.
    if let Schedule::Every(interval) = schedule {
        timeout_secs = timeout_secs.min(interval.secs());
    }

    Ok(CronJobSpec {
        name,
        schedule,
        schedule_spec: args.schedule_spec,
        kind: JobKind::Sh,
        command: args.command.join(" "),
        agent: None,
        cwd: args.cwd.unwrap_or(default_cwd),
        notify: args.notify,
        timeout_secs,
        catchup_secs: args.catchup_secs,
        paused: args.paused,
    })
}

/// Parses `cron edit NAME <options>` into a patch.
///
/// Only the fields the caller actually named are touched — `cron edit NAME`
/// alone is refused rather than silently doing nothing or clearing the job.
pub fn parse_edit(args: &[String]) -> Result<(String, CronJobPatch), String> {
    let name = args.first().ok_or("expected a job name")?.clone();
    let mut patch = CronJobPatch::default();
    let mut index = 1;

    while index < args.len() {
        let option = args[index].as_str();
        if option == "--quiet" {
            patch.notify = Some(NotifyPolicy::Never);
            index += 1;
            continue;
        }
        index += 1;
        let value = args
            .get(index)
            .ok_or(format!("{option} requires a value"))?;
        index += 1;

        match option {
            "--name" => {
                if value.len() > MAX_NAME_LEN || value.is_empty() {
                    return Err(format!("--name must be 1-{MAX_NAME_LEN} characters"));
                }
                patch.name = Some(value.clone())
            }
            "--schedule" => {
                let schedule = parse_schedule_arg(value)?;
                patch.schedule = Some((schedule, value.clone()));
            }
            "--command" => patch.command = Some(value.clone()),
            "--cwd" => patch.cwd = Some(value.clone()),
            "--on" => patch.notify = Some(NotifyPolicy::parse(value)?),
            "--timeout" => patch.timeout_secs = Some(parse_named_duration(value)?),
            "--catchup" => patch.catchup_secs = Some(parse_named_duration(value)?),
            _ => return Err(format!("{option}: unknown option")),
        }
    }

    if patch.is_empty() {
        return Err("nothing to change; name at least one field to edit".to_string());
    }
    Ok((name, patch))
}

/// Current directory, as a string, for defaulting `--cwd`.
pub fn current_dir_string() -> Result<String> {
    Ok(std::env::current_dir()
        .context("cannot read current directory")?
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests;
