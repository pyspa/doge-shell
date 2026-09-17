//! `cron logs`: one run's full recorded `stdout`/`stderr`.
//!
//! Split out of `handlers.rs` purely for size, the same reason `doctor.rs`
//! was. `cron history` shows a table of many runs at a preview-length
//! resolution; this shows one run in full - the two are deliberately
//! different subcommands (see `mod.rs`'s doc comment) rather than a `--full`
//! flag on `history`, because a table row and a full 8 KiB stream do not fit
//! the same rendering.

use super::*;
use dsh_types::cron::job::RunOutput;

struct LogsArgs {
    job: Option<String>,
    run: Option<String>,
    stdout_only: bool,
    stderr_only: bool,
    json: bool,
}

/// `--run` takes a value, so - like `history`'s `--limit` - it cannot be
/// stripped by `without_flags` (built for bare flags): left in the remaining
/// args, its value would be misread as the job-name argument.
fn parse_logs_args(args: &[String]) -> Result<LogsArgs, String> {
    let mut run = None;
    let mut stdout_only = false;
    let mut stderr_only = false;
    let mut json = false;
    let mut job = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--stdout" => stdout_only = true,
            "--stderr" => stderr_only = true,
            "--run" => {
                index += 1;
                let value = args.get(index).ok_or("--run requires a value")?;
                run = Some(value.clone());
            }
            other => {
                if job.is_none() {
                    job = Some(other.to_string());
                }
            }
        }
        index += 1;
    }
    if stdout_only && stderr_only {
        return Err(
            "--stdout and --stderr are mutually exclusive; omit both to see both".to_string(),
        );
    }
    Ok(LogsArgs {
        job,
        run,
        stdout_only,
        stderr_only,
        json,
    })
}

/// `--json`'s value. A pure function of what `logs` has already gathered, so
/// the "only include the stream(s) actually asked for" rule (`--stdout`/
/// `--stderr` narrow `--json` too, matching the non-JSON path) can be tested
/// without a `Context` to write through. Also what `cron_manage(action=logs)`
/// (`dsh/src/cron/tool.rs`) returns, so the two never drift onto different
/// JSON shapes for the same run.
pub(in crate::cron) fn logs_json(
    output: &RunOutput,
    show_stdout: bool,
    show_stderr: bool,
) -> serde_json::Value {
    let mut value = json!({ "run": run_json(&output.run) });
    if show_stdout {
        value["stdout"] = json!(&output.stdout);
    }
    if show_stderr {
        value["stderr"] = json!(output.stderr);
    }
    value
}

pub(in crate::cron) fn logs(ctx: &Context, store: &SqliteCronStore, args: &[String]) -> Result<()> {
    let parsed = parse_logs_args(args).map_err(anyhow::Error::msg)?;
    let selector = match (parsed.run, parsed.job) {
        // Both given: the run must belong to the named job, not just be *a*
        // run id that happens to exist somewhere - see `RunSelector::JobAndId`'s
        // own doc comment for what silently accepting any job's run used to do.
        (Some(run), Some(job)) => RunSelector::JobAndId { job, run },
        (Some(run), None) => RunSelector::Id(run),
        (None, Some(job)) => RunSelector::Latest(job),
        (None, None) => {
            bail!("expected a job name (or --run <id>); see `cron history` for run ids")
        }
    };
    let output = store.run_output(&selector)?;
    let show_stdout = !parsed.stderr_only;
    let show_stderr = !parsed.stdout_only;

    if parsed.json {
        let value = logs_json(&output, show_stdout, show_stderr);
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }

    ctx.write_stdout(&render_run_output(&output, show_stdout, show_stderr))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::cron::job::{CronRun, RunState, RunTrigger};

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn run_output(stdout: &str, stderr: &str) -> RunOutput {
        RunOutput {
            run: CronRun {
                id: "r1".to_string(),
                job_id: 1,
                job_name: "probe".to_string(),
                scheduled_for: 0,
                state: RunState::Succeeded,
                reason: None,
                started_at: Some(0),
                finished_at: Some(1),
                duration_ms: 1_000,
                exit_code: 0,
                timed_out: false,
                changed: false,
                trigger: RunTrigger::Tick,
                agent_task_id: None,
                tokens_used: 0,
                pending_skills: 0,
                preview: String::new(),
            },
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn json_includes_both_streams_by_default() {
        let value = logs_json(&run_output("out", "err"), true, true);
        assert_eq!(value["stdout"], serde_json::json!("out"));
        assert_eq!(value["stderr"], serde_json::json!("err"));
    }

    /// The bug this guards against: `--json --stdout` used to still embed a
    /// populated `stderr` field, unlike the non-JSON path, which does honor
    /// `--stdout`/`--stderr`.
    #[test]
    fn json_with_stdout_only_omits_stderr() {
        let value = logs_json(&run_output("out", "err"), true, false);
        assert_eq!(value["stdout"], serde_json::json!("out"));
        assert!(value.get("stderr").is_none(), "{value}");
    }

    #[test]
    fn json_with_stderr_only_omits_stdout() {
        let value = logs_json(&run_output("out", "err"), false, true);
        assert!(value.get("stdout").is_none(), "{value}");
        assert_eq!(value["stderr"], serde_json::json!("err"));
    }

    #[test]
    fn a_bare_job_name_is_the_job_not_mistaken_for_run() {
        let parsed = parse_logs_args(&args(&["digest"])).unwrap();
        assert_eq!(parsed.job.as_deref(), Some("digest"));
        assert_eq!(parsed.run, None);
    }

    /// The bug this guards against: the same class of mistake `history`'s
    /// `--limit` used to make - a value-taking flag read as (or swallowing)
    /// the job-name argument.
    #[test]
    fn run_takes_a_value_and_is_not_mistaken_for_a_job_name() {
        let parsed = parse_logs_args(&args(&["--run", "abc123"])).unwrap();
        assert_eq!(parsed.run.as_deref(), Some("abc123"));
        assert_eq!(parsed.job, None);
    }

    #[test]
    fn a_job_name_and_run_id_both_parse_regardless_of_order() {
        let parsed = parse_logs_args(&args(&["digest", "--run", "abc123"])).unwrap();
        assert_eq!(parsed.job.as_deref(), Some("digest"));
        assert_eq!(parsed.run.as_deref(), Some("abc123"));

        let parsed = parse_logs_args(&args(&["--run", "abc123", "digest"])).unwrap();
        assert_eq!(parsed.job.as_deref(), Some("digest"));
        assert_eq!(parsed.run.as_deref(), Some("abc123"));
    }

    #[test]
    fn a_missing_run_value_is_a_clear_error() {
        assert!(parse_logs_args(&args(&["--run"])).is_err());
    }

    #[test]
    fn stdout_and_stderr_together_are_refused() {
        assert!(parse_logs_args(&args(&["digest", "--stdout", "--stderr"])).is_err());
    }

    #[test]
    fn json_parses_alongside_a_job_name_in_either_order() {
        let parsed = parse_logs_args(&args(&["--json", "digest"])).unwrap();
        assert!(parsed.json);
        assert_eq!(parsed.job.as_deref(), Some("digest"));

        let parsed = parse_logs_args(&args(&["digest", "--json"])).unwrap();
        assert!(parsed.json);
        assert_eq!(parsed.job.as_deref(), Some("digest"));
    }
}
