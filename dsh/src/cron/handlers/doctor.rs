//! `cron doctor`: diagnostics shared across its text output, `--json`, and
//! the `cron_manage(action=doctor)` chat tool.
//!
//! Split out of `handlers.rs` purely for size, the same reason `handlers.rs`
//! itself was split out of `mod.rs`.

use super::*;

/// What `cron doctor` found, shared between its text and `--json` output and
/// the `cron_manage(action=doctor)` chat tool - so a diagnostic added here
/// reaches all three at once instead of being formatted three times.
pub(in crate::cron) struct DoctorReport {
    pub(in crate::cron) lines: Vec<String>,
    pub(in crate::cron) ok: usize,
    pub(in crate::cron) warn: usize,
}

impl DoctorReport {
    pub(in crate::cron) fn to_json(&self) -> serde_json::Value {
        json!({ "ok": self.ok, "warn": self.warn, "lines": self.lines })
    }
}

pub(in crate::cron) fn doctor_report(
    shell: &mut crate::shell::Shell,
    store: &SqliteCronStore,
    now: i64,
) -> Result<DoctorReport> {
    let mut lines: Vec<String> = Vec::new();
    let mut ok_count = 0usize;
    let mut warn_count = 0usize;

    let jobs = store.list()?;
    if jobs.is_empty() {
        lines.push("ok no-jobs".to_string());
        ok_count += 1;
    }

    let mut any_notify_configured = false;
    for job in &jobs {
        // A schedule the parser accepted syntactically can still never match
        // any real date (`0 0 30 2 *`) - `next_run_at` is `None` from the
        // moment it was created, and the job silently never fires. Nothing
        // else surfaces this; `cron list` shows the job as `ok`.
        if job.next_run_at.is_none()
            && !job.paused
            && !matches!(
                job.schedule,
                dsh_types::schedule::Schedule::AtStartup | dsh_types::schedule::Schedule::Manual
            )
        {
            lines.push(format!(
                "warn {} schedule-never-matches {:?} will not run on its own; edit the schedule",
                job.name, job.schedule_spec
            ));
            warn_count += 1;
        }

        if !std::path::Path::new(&job.cwd).is_dir() {
            lines.push(format!(
                "warn {} cwd-missing {} no longer exists",
                job.name, job.cwd
            ));
            warn_count += 1;
        }

        if job.notify != dsh_types::schedule::NotifyPolicy::Never {
            any_notify_configured = true;
        }

        // A run still holding its claim is not necessarily stuck - a slow
        // job legitimately runs a while - but it is exactly the thing a
        // person asking "why didn't the next run fire" wants pointed out,
        // and nothing else surfaces it (`cron list` shows `running`, not
        // since when).
        //
        // `job.last` is *not* this run: `list()` populates it from
        // `latest_runs()`, which only ever looks at finished runs
        // (`WHERE finished_at IS NOT NULL`) - the in-progress run has
        // `finished_at = NULL` and is excluded, so `job.last` is whatever
        // run happened *before* this one. Using its `started_at` here would
        // report how long a run that already finished took, not how long
        // the current one has actually been running.
        if job.running {
            let current = store
                .runs(&RunQuery {
                    job: Some(job.name.clone()),
                    limit: 1,
                    ..Default::default()
                })
                .ok()
                .and_then(|runs| runs.into_iter().next());
            let since = current.and_then(|run| run.started_at);
            let detail = since.map_or_else(
                || "start time unknown".to_string(),
                |started| {
                    format!(
                        "running for {}",
                        super::super::cli::format_duration((now - started).max(0) as u64)
                    )
                },
            );
            lines.push(format!("warn {} still-running {detail}", job.name));
            warn_count += 1;
        }

        if let Some(agent) = &job.agent {
            if dsh_builtin::agent::resolved_config(shell)
                .api_key()
                .is_none()
            {
                lines.push(format!(
                    "warn {} no-api-key set AI_CHAT_API_KEY before this job's next run",
                    job.name
                ));
                warn_count += 1;
            }
            if !agent.grant.mcp_calls.is_empty() {
                let servers = shell.environment.read().mcp_servers().len();
                if servers == 0 {
                    lines.push(format!(
                        "warn {} mcp-grant-without-servers granted --allow-mcp but config.lisp has no MCP servers configured",
                        job.name
                    ));
                    warn_count += 1;
                } else {
                    lines.push(format!("ok {} mcp-grant-has-servers", job.name));
                    ok_count += 1;
                }
            }
        }
    }

    // `--on` is recorded and shown, but nothing yet delivers a notification
    // for it - `cron history`/`cron status` are the only way to see a run's
    // outcome today. One line regardless of how many jobs set it: this is a
    // standing gap in the feature, not a per-job misconfiguration.
    if any_notify_configured {
        lines.push(
            "note notifications-not-delivered `--on` is recorded but not yet sent anywhere; \
             check `cron history`/`cron status` for a run's outcome"
                .to_string(),
        );
    }

    // `sched-add` is a deprecated alias; a config.lisp that still calls it
    // works (it upserts, same as `cron-add`) but should migrate before the
    // alias is removed.
    let config_path = dsh_builtin::config_paths::config_file("config.lisp");
    if let Ok(contents) = std::fs::read_to_string(&config_path)
        && contents.contains("sched-add")
    {
        lines.push(format!(
            "warn config.lisp still-calls-sched-add replace with cron-add in {}",
            config_path.display()
        ));
        warn_count += 1;
    } else {
        lines.push("ok config.lisp no-sched-add".to_string());
        ok_count += 1;
    }

    // If jobs exist but nothing has ever completed, either no tick has
    // arrived yet or every attempt failed to even start.
    let health = store.health(now)?;
    if health.total > 0 && health.last_run_at.is_none() {
        lines.push(
            "warn no-run-has-ever-completed is a tick arriving? see `cron setup`".to_string(),
        );
        warn_count += 1;
    } else if health.last_run_at.is_some() {
        lines.push("ok a-run-has-completed".to_string());
        ok_count += 1;
    }

    Ok(DoctorReport {
        lines,
        ok: ok_count,
        warn: warn_count,
    })
}

pub(in crate::cron) fn doctor(
    shell: &mut crate::shell::Shell,
    ctx: &Context,
    store: &SqliteCronStore,
    args: &[String],
) -> Result<()> {
    let json_output = has_flag(args, "--json");
    let report = doctor_report(shell, store, now())?;

    if json_output {
        ctx.write_stdout(&serde_json::to_string_pretty(&report.to_json())?)?;
        ctx.write_stdout("\n")?;
        return Ok(());
    }
    for line in &report.lines {
        ctx.write_stdout(&format!("{line}\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this guards against: `doctor_report`'s "still-running" line
    /// used to read `job.last.started_at` - which `list()` populates only
    /// from *finished* runs (`WHERE finished_at IS NOT NULL`), so while a
    /// job is running it names its previous, already-finished run instead of
    /// the one actually in progress.
    #[test]
    fn still_running_reports_the_current_runs_duration_not_a_previous_ones() {
        use dsh_types::cron::job::{RunOutcome, RunState};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = SqliteCronStore::open(&dir.path().join("cron")).expect("open");
        let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());

        let parsed = parse_add(&["1h".to_string(), "true".to_string()]).unwrap();
        let spec = build_spec(parsed, "/tmp".to_string()).unwrap();
        let base = 1_700_000_000i64;
        store
            .create(&spec, &std::collections::HashMap::new(), base, false)
            .unwrap();

        // A finished run long before the one that matters below - if
        // `doctor_report` ever goes back to reading `job.last`, its
        // "running for" duration will silently come from this one instead.
        store.trigger("true", base).unwrap();
        let first = store
            .claim_due(base, "owner", 10, RunTrigger::Tick)
            .unwrap();
        store.start(&first[0].run_id, base).unwrap();
        store
            .complete(
                &first[0].run_id,
                &RunOutcome {
                    state: RunState::Succeeded,
                    ..Default::default()
                },
                base + 5,
            )
            .unwrap();

        let started_at = base + 10_000;
        store.trigger("true", started_at).unwrap();
        let second = store
            .claim_due(started_at, "owner", 10, RunTrigger::Tick)
            .unwrap();
        store.start(&second[0].run_id, started_at).unwrap();
        // Left running - no `complete` call.

        let now = started_at + 90;
        let report = doctor_report(&mut shell, &store, now).unwrap();
        let line = report
            .lines
            .iter()
            .find(|line| line.contains("still-running"))
            .unwrap_or_else(|| panic!("no still-running line in {:?}", report.lines));
        assert!(
            line.contains("running for 1m30s"),
            "expected the current run's duration (90s), got: {line}"
        );
    }
}
