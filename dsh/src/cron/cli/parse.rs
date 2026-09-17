//! Turning `cron` argv into store-ready specs.

use anyhow::{Context as _, Result};
use dsh_builtin::agent::grant::apply_grant_option;
use dsh_types::agent::{TaskGrant, Verification};
use dsh_types::cron::job::{AgentJobSpec, CronJobPatch, CronJobSpec, JobKind};
use dsh_types::schedule::{DEFAULT_TIMEOUT_SECS, NotifyPolicy, Schedule, parse_schedule};

use crate::agent::DEFAULT_AGENT_TIMEOUT_SECS;

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
    pub agent: bool,
    pub grant: TaskGrant,
    pub criteria: Vec<String>,
    pub max_tokens_per_day: Option<u64>,
    pub schedule_spec: String,
    pub schedule: Option<Schedule>,
    /// The command line for a shell job, or the goal for an agent job.
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

/// Parses `cron add`'s options, then the schedule, then the command or goal.
///
/// Mirrors `agent run`'s option loop deliberately: everything after `--check`
/// is the same flag, the same validation, the same error text, because this
/// is the same grant a person would otherwise type by hand.
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
        if option == "--agent" {
            out.agent = true;
            index += 1;
            continue;
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
        if option == "--sandbox" {
            out.grant.sandbox = true;
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

        if apply_grant_option(&mut out.grant, option, value).map_err(|error| error.to_string())? {
            continue;
        }
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
            "--max-tokens-per-day" => {
                out.max_tokens_per_day = Some(
                    value
                        .parse()
                        .map_err(|_| "--max-tokens-per-day must be a number".to_string())?,
                )
            }
            "--check" => out.criteria.push(value.clone()),
            _ => return Err(format!("{option}: unknown option")),
        }
    }

    let schedule_spec = args
        .get(index)
        .ok_or("expected a schedule (e.g. '5m' or '0 9 * * *')")?;
    out.schedule = Some(parse_schedule_arg(schedule_spec)?);
    out.schedule_spec = schedule_spec.clone();
    index += 1;

    // An optional `--` separates options from the command/goal, the same as
    // any other subcommand's argv. It matters more for an agent job — whose
    // goal is free text that might otherwise start with something that looks
    // like an option — but a shell job accepts it too, rather than silently
    // folding a stray `--` into the command line it runs (`sh -c '-- exit 3'`
    // is not `exit 3`; it is `sh` complaining about `--` as an option).
    let rest = &args[index..];
    let command = match rest.first().map(String::as_str) {
        Some("--") => rest[1..].to_vec(),
        _ => rest.to_vec(),
    };
    if command.is_empty() {
        return Err(if out.agent {
            "expected a goal after --".to_string()
        } else {
            "expected a command to run".to_string()
        });
    }
    out.command = command;
    Ok(out)
}

/// The name a job gets when `--name` was not given.
///
/// A shell job takes its first word, matching `sched`'s old default. An
/// agent job's "first word" would usually be an article, so it takes the
/// first few words of the goal instead and slugs them into something safe to
/// use as a filename.
pub fn default_job_name(agent: bool, command: &[String]) -> String {
    let joined = command.join(" ");
    if !agent {
        return joined
            .split_whitespace()
            .next()
            .unwrap_or("job")
            .to_string();
    }
    let slug: String = joined
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    let capped: String = trimmed.chars().take(MAX_NAME_LEN).collect();
    if capped.is_empty() {
        "job".to_string()
    } else {
        capped
    }
}

/// Builds the spec `cron add` hands to the store, given the parsed options
/// and the current directory to default `--cwd` to.
pub fn build_spec(args: AddArgs, default_cwd: String) -> Result<CronJobSpec, String> {
    let name = args
        .name
        .unwrap_or_else(|| default_job_name(args.agent, &args.command));

    let schedule = args
        .schedule
        .expect("schedule is always parsed by parse_add");
    let mut timeout_secs = args.timeout_secs.unwrap_or(if args.agent {
        // An agent job's `--timeout` doubles as the task's own time budget,
        // so it shares `agent run`'s default rather than the shell-job one.
        DEFAULT_AGENT_TIMEOUT_SECS
    } else {
        DEFAULT_TIMEOUT_SECS
    });
    // An interval job that outlives its own interval would starve its own
    // next run; a cron expression has no fixed interval to compare against.
    if let Schedule::Every(interval) = schedule {
        timeout_secs = timeout_secs.min(interval.secs());
    }

    let (kind, command, agent) = if args.agent {
        if args.grant.read_roots.is_empty() && args.grant.write_roots.is_empty() {
            return Err(
                "an agent job needs at least one --read or --write; nothing is granted by default"
                    .to_string(),
            );
        }
        (
            JobKind::Ai,
            args.command.join(" "),
            Some(AgentJobSpec {
                grant: args.grant,
                criteria: args.criteria,
                // One flag drives both the model's own cooperative budget and
                // the outer wall-clock deadline the tick enforces from
                // outside; see `run_job`'s doc comment for why they are kept
                // in lock-step rather than given two names.
                time_budget_secs: timeout_secs,
                max_tokens_per_day: args.max_tokens_per_day,
            }),
        )
    } else {
        (JobKind::Sh, args.command.join(" "), None)
    };

    Ok(CronJobSpec {
        name,
        schedule,
        schedule_spec: args.schedule_spec,
        kind,
        command,
        agent,
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
/// `existing_agent` is the job's current AI payload, if it has one. `patch`
/// stores a whole new `AgentJobSpec` rather than diffing columns (see
/// `store/api.rs::patch`), so touching *any* grant-related flag here has to
/// start from what is already there - otherwise `cron edit job --check
/// '...'` would silently drop every `--read`/`--write`/`--allow-command`/
/// budget the job already had.
pub fn parse_edit(
    args: &[String],
    existing_agent: Option<&AgentJobSpec>,
) -> Result<(String, CronJobPatch), String> {
    let name = args.first().ok_or("expected a job name")?.clone();
    let mut patch = CronJobPatch::default();
    let mut grant = existing_agent
        .map(|agent| agent.grant.clone())
        .unwrap_or_default();
    // Whether a flag that only makes sense on an agent job (a grant flag,
    // `--max-tokens-per-day`, `--check`, `--sandbox`) was named.
    // `--timeout` is deliberately *not* one of these here - it is checked
    // separately below, because it is valid on every job but, on an agent
    // job specifically, still has to resync `time_budget_secs` (see below).
    let mut agent_flag_touched = false;
    let mut criteria: Vec<String> = existing_agent
        .map(|agent| agent.criteria.clone())
        .unwrap_or_default();
    let mut max_tokens_per_day = existing_agent.and_then(|agent| agent.max_tokens_per_day);
    let mut index = 1;

    while index < args.len() {
        let option = args[index].as_str();
        if option == "--sandbox" {
            grant.sandbox = true;
            agent_flag_touched = true;
            index += 1;
            continue;
        }
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

        if apply_grant_option(&mut grant, option, value).map_err(|error| error.to_string())? {
            agent_flag_touched = true;
            continue;
        }
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
            "--command" | "--goal" => patch.command = Some(value.clone()),
            "--cwd" => patch.cwd = Some(value.clone()),
            "--on" => patch.notify = Some(NotifyPolicy::parse(value)?),
            "--timeout" => patch.timeout_secs = Some(parse_named_duration(value)?),
            "--catchup" => patch.catchup_secs = Some(parse_named_duration(value)?),
            "--max-tokens-per-day" => {
                max_tokens_per_day = Some(
                    value
                        .parse()
                        .map_err(|_| "--max-tokens-per-day must be a number".to_string())?,
                );
                agent_flag_touched = true;
            }
            "--check" => {
                criteria.push(value.clone());
                agent_flag_touched = true;
            }
            _ => return Err(format!("{option}: unknown option")),
        }
    }

    // Agent-only flags on a job with no existing agent spec would otherwise
    // silently fabricate one (an empty grant) on what is
    // really a shell job: `job.kind` stays `sh`, but `cron show`/`doctor`
    // would start rendering a bogus "agent:" section for it.
    if agent_flag_touched && existing_agent.is_none() {
        return Err(
            "this job is not an agent job; --read/--write/--allow-command/--allow-mcp/--network/\
             --env/--sandbox/--max-tokens-per-day/--check only apply to one created \
             with `cron add --agent`"
                .to_string(),
        );
    }

    // An agent job's `time_budget_secs` must track `timeout_secs` exactly -
    // the claim lease (`store/claim.rs`) and the watchdog
    // (`run_job.rs::arm_watchdog`) are both derived from `lease_secs`, one
    // from `jobs.timeout_secs` and the other from this stored value, and the
    // two must stay in lock-step or a claim can be reaped while the process
    // it belongs to is still legitimately running. So an edit that only
    // touches `--timeout` on an existing agent job still has to rebuild
    // `patch.agent`, even though `--timeout` alone does not set
    // `agent_flag_touched`.
    if existing_agent.is_some() && (agent_flag_touched || patch.timeout_secs.is_some()) {
        let existing_time_budget_secs = existing_agent
            .map(|agent| agent.time_budget_secs)
            .unwrap_or(0);
        patch.agent = Some(AgentJobSpec {
            grant,
            criteria,
            time_budget_secs: patch.timeout_secs.unwrap_or(existing_time_budget_secs),
            max_tokens_per_day,
        });
    }

    if patch.is_empty() {
        return Err("nothing to change; name at least one field to edit".to_string());
    }
    Ok((name, patch))
}

pub fn criteria_to_verifications(criteria: &[String]) -> Vec<Verification> {
    criteria
        .iter()
        .map(|criterion| Verification {
            criterion: criterion.clone(),
            evidence_event: None,
            passed: false,
        })
        .collect()
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
