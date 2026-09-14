//! Subcommand bodies for [`super::command`].
//!
//! Split out of `mod.rs` purely for size; every function here takes the
//! already-opened store and writes through `ctx`, the same shape `sched.rs`
//! and `agent.rs` use.

use anyhow::{Context as _, Result, bail};
use dsh_builtin::shell_capabilities::CronStore;
use dsh_types::Context;
use dsh_types::cron::job::{RunQuery, RunSelector, RunTrigger};
use serde_json::json;

use super::cli::{
    build_spec, current_dir_string, parse_add, parse_edit, render_history, render_incidents,
    render_job_list, render_run_output,
};
use super::store::SqliteCronStore;
use super::tick;

mod doctor;
pub(in crate::cron) use doctor::{doctor, doctor_report};
mod logs;
pub(in crate::cron) use logs::{live_agent_summary, logs, logs_json};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn without_flags<'a>(args: &'a [String], flags: &[&str]) -> Vec<&'a String> {
    args.iter()
        .filter(|a| !flags.contains(&a.as_str()))
        .collect()
}

pub(super) fn add(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let parsed = parse_add(args).map_err(anyhow::Error::msg)?;
    let force = parsed.force;
    let cwd = current_dir_string()?;
    let spec = build_spec(parsed, cwd).map_err(anyhow::Error::msg)?;
    let summary = format!("{} {} -> {}", spec.name, spec.schedule, spec.command);
    let id = store.create(&spec, &std::env::vars().collect(), now(), force)?;
    ctx.write_stdout(&format!("cron: [{id}] {summary}"))?;
    Ok(())
}

pub(super) fn list(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let jobs = store.list()?;
    if has_flag(args, "--json") {
        let value: Vec<_> = jobs.iter().map(job_json).collect();
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }
    ctx.write_stdout(&render_job_list(&jobs, now()))?;
    Ok(())
}

pub(super) fn job_json(job: &dsh_types::cron::job::CronJobView) -> serde_json::Value {
    json!({
        "id": job.id,
        "name": job.name,
        "kind": job.kind.as_str(),
        "schedule": job.schedule_spec,
        "command": job.command,
        "cwd": job.cwd,
        "notify": job.notify.as_str(),
        "timeout_secs": job.timeout_secs,
        "catchup_secs": job.catchup_secs,
        "paused": job.paused,
        "blocked": job.blocked,
        "next_run_at": job.next_run_at,
        "running": job.running,
        "run_count": job.run_count,
        "fail_count": job.fail_count,
        "consecutive_failures": job.consecutive_failures,
        "state": job.state_label(),
    })
}

/// The `show`/`cron_manage(action=show)` detail view: [`job_json`] plus the
/// notepad path, the agent payload `list` leaves out, and its most recent
/// run - already loaded by `store.get()` (`job.last`) but, until now, never
/// actually shown here.
pub(super) fn job_detail_json(
    job: &dsh_types::cron::job::CronJobView,
    notepad_path: &std::path::Path,
) -> serde_json::Value {
    let mut value = job_json(job);
    value["notepad_path"] = json!(notepad_path.to_string_lossy());
    value["last"] = job.last.as_ref().map(run_json).into();
    if let Some(agent) = &job.agent {
        value["agent"] = json!({
            "grant": agent.grant,
            "criteria": agent.criteria,
            "token_budget": agent.token_budget,
            "time_budget_secs": agent.time_budget_secs,
            "max_tokens_per_day": agent.max_tokens_per_day,
        });
    }
    value
}

pub(super) fn show(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    // `--json` must be stripped before taking the first positional - unlike
    // `edit` (whose job name is a documented "always first" convention with
    // no competing flag), `show` has a real `--json` option, so `cron show
    // --json probe` would otherwise read `--json` itself as the job name.
    let rest = without_flags(args, &["--json"]);
    let name = rest.first().context("expected a job name")?;
    let job = store.get(name)?;
    let notepad_path = store.notepad_path(&job.name);
    if has_flag(args, "--json") {
        let value = job_detail_json(&job, &notepad_path);
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }

    ctx.write_stdout(&format!("{}: {}", job.name, job.schedule))?;
    ctx.write_stdout(&format!("  kind: {}", job.kind))?;
    ctx.write_stdout(&format!("  command: {}", job.command))?;
    ctx.write_stdout(&format!("  cwd: {}", job.cwd))?;
    ctx.write_stdout(&format!("  notify: {}", job.notify))?;
    ctx.write_stdout(&format!("  timeout: {}s", job.timeout_secs))?;
    ctx.write_stdout(&format!("  state: {}", job.state_label()))?;
    ctx.write_stdout(&format!("  notepad: {}", notepad_path.display()))?;
    if let Some(agent) = &job.agent {
        ctx.write_stdout(&format!(
            "  agent: tokens={} time_budget={}s max_per_day={}",
            agent.token_budget,
            agent.time_budget_secs,
            agent
                .max_tokens_per_day
                .map_or("-".to_string(), |n| n.to_string())
        ))?;
        for criterion in &agent.criteria {
            ctx.write_stdout(&format!("  check: {criterion}"))?;
        }
    }
    if let Some(last) = &job.last {
        ctx.write_stdout(&format!(
            "  last run: {} {:.1}s (run {})",
            last.state,
            last.duration_ms as f64 / 1000.0,
            last.id
        ))?;
        if let Some(task) = &last.agent_task_id {
            ctx.write_stdout(&format!("    task: {task}"))?;
        }
        if !last.preview.is_empty() {
            ctx.write_stdout(&format!("    {}", last.preview))?;
        }
        // `job.last` (from `store.get()`) is not filtered to finished runs,
        // unlike `cron logs`'s own `RunSelector::Latest` - so this hint
        // would point at a run `cron logs` then refuses ("no finished run
        // recorded yet") if it were shown for one still queued/running.
        if last.finished_at.is_some() {
            ctx.write_stdout(&format!("  (full output: cron logs {})", job.name))?;
        }
    }
    Ok(())
}

/// The job's current AI payload, if any, for `parse_edit` to merge onto.
///
/// An unknown job is not fatal here - `parse_edit` still works, and the
/// unknown-job error is `store.patch`'s own `resolve` to report, with a
/// clearer message than this function could give. Anything else - most
/// plausibly a `payload` column that fails to deserialise, from a corrupt row
/// or a schema change - is a real problem, though, and has to propagate: if
/// this silently answered `None` the same way "not found" does, `parse_edit`
/// would treat the job as having no existing agent spec and, on any
/// grant-touching flag, refuse the edit outright (see `parse_edit`'s
/// `agent_flag_touched` check) rather than losing anything - but it used to
/// build a *replacement* `AgentJobSpec` from empty defaults, which
/// `store.patch` would then write over whatever grant the job actually had.
pub(super) fn existing_agent_for_edit(
    store: &SqliteCronStore,
    selector: &str,
) -> Result<Option<dsh_types::cron::job::AgentJobSpec>> {
    match store.get(selector) {
        Ok(job) => Ok(job.agent),
        Err(error) if error.to_string().contains("no such cron job") => Ok(None),
        Err(error) => Err(error).context("cron edit: could not read the job's current definition"),
    }
}

pub(super) fn edit(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let job_name = args.first().context("expected a job name")?.clone();
    let existing_agent = existing_agent_for_edit(store, &job_name)?;
    let (name, patch) = parse_edit(args, existing_agent.as_ref()).map_err(anyhow::Error::msg)?;
    let name = store.patch(&name, &patch, now())?;
    ctx.write_stdout(&format!("cron: {name} updated"))?;
    Ok(())
}

pub(super) fn remove(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("expected at least one job name");
    }
    for selector in args {
        let name = store.delete(selector)?;
        ctx.write_stdout(&format!("cron: removed {name}"))?;
    }
    Ok(())
}

pub(super) fn set_paused(
    ctx: &Context,
    store: &SqliteCronStore,
    args: &[String],
    paused: bool,
) -> Result<()> {
    let verb = if paused { "paused" } else { "resumed" };
    if args.is_empty() {
        let count = store.set_all_paused(paused, now())?;
        ctx.write_stdout(&format!("cron: {count} job(s) {verb}"))?;
        return Ok(());
    }
    for selector in args {
        let name = store.set_paused(selector, paused, now())?;
        ctx.write_stdout(&format!("cron: {name} {verb}"))?;
    }
    Ok(())
}

pub(super) fn run(
    _shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &SqliteCronStore,
    args: &[String],
) -> Result<()> {
    let foreground = has_flag(args, "--now");
    let rest = without_flags(args, &["--now"]);
    let name = rest.first().context("expected a job name")?;

    if !foreground {
        let name = store.trigger(name, now())?;
        ctx.write_stdout(&format!("cron: {name} will run on the next tick"))?;
        return Ok(());
    }

    let owner = tick::owner_id();
    let claimed = store.claim_one(name, now(), &owner, RunTrigger::Manual)?;
    let child = super::exec::spawn_run_child(&claimed.run_id).context("cannot start the run")?;
    let output = child
        .wait_with_output()
        .context("cannot wait for the run to finish")?;
    if !output.status.success() {
        ctx.write_stdout(&format!(
            "cron: run-job exited with {:?}; showing what was recorded",
            output.status.code()
        ))?;
    }

    // The child recorded its own outcome; read it back rather than trusting
    // the child's own exit code, which only reflects `run-job` itself.
    let runs = store.runs(&RunQuery {
        job: Some(claimed.job_name.clone()),
        limit: 1,
        ..Default::default()
    })?;
    if let Some(latest) = runs.first() {
        ctx.write_stdout(&format!("cron: {} -> {}", claimed.job_name, latest.state))?;
        if !latest.preview.is_empty() {
            ctx.write_stdout(&latest.preview)?;
        }
        ctx.write_stdout(&format!(
            "cron: full output: cron logs {}",
            claimed.job_name
        ))?;
    }
    Ok(())
}

/// Runs shown by `cron history` when `--limit` is not given.
const DEFAULT_HISTORY_LIMIT: usize = 20;

struct HistoryArgs {
    job: Option<String>,
    limit: usize,
    json: bool,
    failed_only: bool,
}

/// `--limit` takes a value, so it cannot be stripped by `without_flags`
/// (built for bare flags): left in the remaining args, it used to be either
/// misread as the job-name argument (`cron history --limit 5` -> "no such
/// cron job --limit") or, with a job name given first, silently ignored
/// along with its value.
fn parse_history_args(args: &[String]) -> Result<HistoryArgs, String> {
    let mut limit = DEFAULT_HISTORY_LIMIT;
    let mut json = false;
    let mut failed_only = false;
    let mut job = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--failed" => failed_only = true,
            "--limit" => {
                index += 1;
                let value = args.get(index).ok_or("--limit requires a value")?;
                limit = value
                    .parse()
                    .map_err(|_| format!("--limit must be a number, not {value:?}"))?;
            }
            other => {
                if job.is_none() {
                    job = Some(other.to_string());
                }
            }
        }
        index += 1;
    }
    Ok(HistoryArgs {
        job,
        limit,
        json,
        failed_only,
    })
}

pub(super) fn run_json(run: &dsh_types::cron::job::CronRun) -> serde_json::Value {
    json!({
        "id": run.id, "job": run.job_name, "state": run.state.as_str(),
        "reason": run.reason.map(|r| r.as_str()),
        "started_at": run.started_at, "finished_at": run.finished_at,
        "duration_ms": run.duration_ms, "exit_code": run.exit_code,
        "timed_out": run.timed_out, "changed": run.changed,
        "trigger": run.trigger.as_str(), "agent_task_id": run.agent_task_id,
        "tokens_used": run.tokens_used, "preview": run.preview,
    })
}

pub(super) fn history(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let parsed = parse_history_args(args).map_err(anyhow::Error::msg)?;
    let json_output = parsed.json;
    let runs = store.runs(&RunQuery {
        job: parsed.job,
        limit: parsed.limit,
        finished_only: true,
        failed_only: parsed.failed_only,
    })?;
    if json_output {
        let value: Vec<_> = runs.iter().map(run_json).collect();
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }
    ctx.write_stdout(&render_history(&runs))?;
    Ok(())
}

pub(super) fn ack_incident(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .context("expected an incident id (see `cron incidents`)")?
        .parse()
        .context("incident id must be a number")?;
    let incident = store.ack_incident(id, now())?;
    ctx.write_stdout(&format!(
        "cron: incident {} acknowledged ({})",
        incident.id, incident.kind
    ))?;
    Ok(())
}

pub(super) fn incident_json(incident: &dsh_types::cron::job::CronIncident) -> serde_json::Value {
    json!({
        "id": incident.id, "job": incident.job_name, "kind": incident.kind.as_str(),
        "detail": incident.detail, "agent_task_id": incident.agent_task_id,
        "opened_at": incident.opened_at,
    })
}

pub(super) fn incidents(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let json_output = has_flag(args, "--json");
    let open = store.incidents(true, 50)?;
    if json_output {
        let value: Vec<_> = open.iter().map(incident_json).collect();
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }
    ctx.write_stdout(&render_incidents(&open))?;
    Ok(())
}

pub(super) fn notepad(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let clear = has_flag(args, "--clear");
    let rest = without_flags(args, &["--clear"]);
    let name = rest.first().context("expected a job name")?;
    if clear {
        store.set_notepad(name, "")?;
        ctx.write_stdout(&format!("cron: {name}'s notepad cleared"))?;
        return Ok(());
    }
    let body = store.notepad(name)?;
    if body.is_empty() {
        ctx.write_stdout(&format!("cron: {name} has no notepad yet"))?;
    } else {
        // The file itself may or may not already end in a newline;
        // `write_stdout` always adds exactly one (`writeln!`), so strip any
        // the file has of its own or this doubles up.
        ctx.write_stdout(body.trim_end_matches('\n'))?;
    }
    Ok(())
}

pub(super) fn health_json(
    health: &dsh_types::cron::job::CronHealth,
    store_root: &std::path::Path,
) -> serde_json::Value {
    json!({
        "jobs": health.total, "running": health.running, "failing": health.failing,
        "paused": health.paused, "blocked": health.blocked,
        "open_incidents": health.open_incidents, "overdue": health.overdue,
        "last_run_at": health.last_run_at, "store": store_root.to_string_lossy(),
    })
}

pub(super) fn status(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let health = store.health(now())?;
    if has_flag(args, "--json") {
        let value = health_json(&health, store.root());
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }

    ctx.write_stdout(&format!(
        "cron: {} job(s), {} running, {} failing, {} paused, {} blocked",
        health.total, health.running, health.failing, health.paused, health.blocked
    ))?;
    if health.open_incidents > 0 {
        ctx.write_stdout(&format!(
            "cron: {} open incident(s); see `cron incidents`",
            health.open_incidents
        ))?;
    }
    match health.last_run_at {
        Some(at) => {
            let age = (now() - at).max(0);
            ctx.write_stdout(&format!(
                "cron: last recorded run finished {} ago",
                super::cli::format_duration(age as u64)
            ))?;
        }
        None => {
            ctx.write_stdout(
                "cron: no run has ever completed. If you added a job with a wall-clock \
                 schedule, make sure either a dsh session is open or an external tick is \
                 installed (`cron setup`).",
            )?;
        }
    }
    if health.overdue > 0 {
        ctx.write_stdout(&format!(
            "cron: {} job(s) are overdue; is a tick actually arriving? (`cron setup`)",
            health.overdue
        ))?;
    }
    Ok(())
}

pub(super) fn tick_cmd(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let json_output = has_flag(args, "--json");
    let verbose = has_flag(args, "--verbose");
    let dry_run = has_flag(args, "--dry-run");
    let rest = without_flags(args, &["--json", "--verbose", "--dry-run"]);
    let max = rest
        .iter()
        .position(|a| *a == "--max")
        .and_then(|i| rest.get(i + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(tick::MAX_PER_TICK);
    let at = now();

    if dry_run {
        let due: Vec<_> = store
            .list()?
            .into_iter()
            .filter(|job| !job.paused && !job.blocked && job.next_run_at.is_some_and(|n| n <= at))
            .map(|job| job.name)
            .collect();
        if json_output {
            ctx.write_stdout(&serde_json::to_string_pretty(&json!({"would_run": due}))?)?;
        } else if verbose {
            for name in &due {
                ctx.write_stdout(&format!("would run: {name}"))?;
            }
        }
        return Ok(());
    }

    // A job's own failure is not this command's failure: system cron would
    // otherwise mail the owner every time any job failed, and the fix people
    // reach for is disabling the tick entirely.
    let report = tick::run_once(store, &tick::owner_id(), max, RunTrigger::Tick, at)?;

    if json_output {
        let value = json!({"ran": report.started, "errors": report.spawn_failed});
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
    } else if verbose {
        for name in &report.started {
            ctx.write_stdout(&format!("started: {name}"))?;
        }
        for (name, error) in &report.spawn_failed {
            ctx.write_stderr(&format!("cron: {name}: {error}"))?;
        }
    }
    Ok(())
}

pub(super) fn run_job(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    args: &[String],
) -> Result<()> {
    let run_id = args.first().context("expected a run id")?;
    super::run_job::execute(shell, ctx, run_id)
}

/// `cron incidents ack <ID>` and the bare `cron incidents ack` (missing id)
/// both go through [`incidents`]; this exists only so the outer dispatch can
/// tell "ack" apart from a plain listing before parsing flags.
pub(super) fn is_ack(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("ack")
}

pub(super) fn ack_args(args: &[String]) -> Vec<String> {
    args[1.min(args.len())..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn limit_defaults_when_not_given() {
        let parsed = parse_history_args(&args(&[])).unwrap();
        assert_eq!(parsed.limit, DEFAULT_HISTORY_LIMIT);
        assert_eq!(parsed.job, None);
    }

    /// The bug this guards against: `--limit` was never excluded from the
    /// job-name slot, so a bare `cron history --limit 5` used to read
    /// `--limit` itself as the job name.
    #[test]
    fn limit_is_not_mistaken_for_a_job_name() {
        let parsed = parse_history_args(&args(&["--limit", "5"])).unwrap();
        assert_eq!(parsed.limit, 5);
        assert_eq!(parsed.job, None);
    }

    #[test]
    fn a_job_name_and_limit_both_parse_regardless_of_order() {
        let parsed = parse_history_args(&args(&["probe", "--limit", "3"])).unwrap();
        assert_eq!(parsed.job.as_deref(), Some("probe"));
        assert_eq!(parsed.limit, 3);

        let parsed = parse_history_args(&args(&["--limit", "3", "probe"])).unwrap();
        assert_eq!(parsed.job.as_deref(), Some("probe"));
        assert_eq!(parsed.limit, 3);
    }

    #[test]
    fn json_and_failed_still_parse_alongside_limit() {
        let parsed = parse_history_args(&args(&["--json", "--failed", "--limit", "7"])).unwrap();
        assert!(parsed.json);
        assert!(parsed.failed_only);
        assert_eq!(parsed.limit, 7);
    }

    #[test]
    fn a_missing_limit_value_is_a_clear_error() {
        assert!(parse_history_args(&args(&["--limit"])).is_err());
    }

    #[test]
    fn a_non_numeric_limit_is_a_clear_error() {
        assert!(parse_history_args(&args(&["--limit", "many"])).is_err());
    }

    /// The bug this guards against: `existing_agent_for_edit` used to be
    /// `store.get(...).ok().and_then(...)`, which read *any* store error -
    /// not just "no such job" - as "no existing agent spec". `parse_edit`
    /// then built a brand-new, empty `AgentJobSpec` from that `None` on any
    /// grant-touching flag, and `store.patch` wrote it over whatever grant
    /// the job actually had.
    #[test]
    fn existing_agent_for_edit_treats_unknown_job_as_no_agent_but_propagates_a_real_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SqliteCronStore::open(&dir.path().join("cron")).expect("open");

        assert!(
            existing_agent_for_edit(&store, "nope").unwrap().is_none(),
            "an unknown job must be treated as having no agent spec"
        );

        let parsed = parse_add(&args(&[
            "--agent", "--tokens", "10", "--write", "/tmp", "1h", "--", "goal",
        ]))
        .unwrap();
        let spec = build_spec(parsed, "/tmp".to_string()).unwrap();
        store
            .create(&spec, &std::collections::HashMap::new(), 0, false)
            .unwrap();
        assert!(
            existing_agent_for_edit(&store, &spec.name)
                .unwrap()
                .is_some()
        );

        // Corrupt the row's payload directly - only reachable in practice
        // through a schema change or on-disk corruption, but `parse_edit`
        // must not read it as "no agent spec" once it is.
        let connection = rusqlite::Connection::open(dir.path().join("cron").join("jobs.sqlite3"))
            .expect("raw connection");
        connection
            .execute(
                "UPDATE jobs SET payload = 'not json' WHERE name = ?1",
                [&spec.name],
            )
            .unwrap();

        let error = existing_agent_for_edit(&store, &spec.name).unwrap_err();
        assert!(
            error.to_string().contains("could not read"),
            "a corrupt payload must be a real error, not a silent None: {error}"
        );
    }

    /// `store.get()` has always loaded `job.last`; before this, `show` and
    /// `cron_manage(action=show)` both silently dropped it on the floor.
    #[test]
    fn job_detail_json_includes_the_last_run_when_there_is_one() {
        use dsh_types::cron::job::{RunOutcome, RunState};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = SqliteCronStore::open(&dir.path().join("cron")).expect("open");
        let parsed = parse_add(&args(&["1h", "echo", "hi"])).unwrap();
        let spec = build_spec(parsed, "/tmp".to_string()).unwrap();
        store
            .create(&spec, &std::collections::HashMap::new(), 0, false)
            .unwrap();

        let job = store.get(&spec.name).unwrap();
        let notepad_path = store.notepad_path(&job.name);
        assert!(
            job_detail_json(&job, &notepad_path)["last"].is_null(),
            "a job that has never run has no `last`"
        );

        store.trigger(&spec.name, 0).unwrap();
        let claimed = store.claim_due(0, "owner", 10, RunTrigger::Tick).unwrap();
        store.start(&claimed[0].run_id, 0).unwrap();
        store
            .complete(
                &claimed[0].run_id,
                &RunOutcome {
                    state: RunState::Succeeded,
                    stdout: "hi\n".to_string(),
                    digest: Some(1),
                    ..Default::default()
                },
                1,
            )
            .unwrap();

        let job = store.get(&spec.name).unwrap();
        let value = job_detail_json(&job, &notepad_path);
        assert_eq!(value["last"]["state"], "succeeded");
    }
}
