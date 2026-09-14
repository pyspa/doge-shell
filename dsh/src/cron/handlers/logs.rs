//! `cron logs`: one run's full recorded `stdout`/`stderr`.
//!
//! Split out of `handlers.rs` purely for size, the same reason `doctor.rs`
//! was. `cron history` shows a table of many runs at a preview-length
//! resolution; this shows one run in full - the two are deliberately
//! different subcommands (see `mod.rs`'s doc comment) rather than a `--full`
//! flag on `history`, because a table row and a full 8 KiB stream do not fit
//! the same rendering.

use super::*;
use crate::agent::{SqliteTaskStore, summary};
use crate::cron::cli::render::stream_section;
use dsh_builtin::shell_capabilities::AgentTaskStore;
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
    live: Option<&str>,
    show_stdout: bool,
    show_stderr: bool,
) -> serde_json::Value {
    let mut value = json!({ "run": run_json(&output.run), "reconstructed": live.is_some() });
    if show_stdout {
        value["stdout"] = json!(live.unwrap_or(&output.stdout));
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

    // An AI job's process, killed by its own watchdog before it could ever
    // call `complete`, never wrote a summary into `stdout` at all - but the
    // task itself is still fully recorded in the agent store (see
    // `CronStore::attach_agent_task`'s own doc comment for why the task id
    // survives that anyway). Reconstruct the same summary live instead of
    // showing an empty stream.
    //
    // This also fires for a run that is genuinely still queued/running (its
    // `stdout` is empty for the ordinary reason - `complete` has not run
    // yet), which a store row alone cannot tell apart from one whose process
    // already died: the banner below is worded to be true of both, rather
    // than asserting the run is dead.
    let agent_root = dsh_builtin::config_paths::agent_state_dir();
    let live = (show_stdout && output.stdout.is_empty())
        .then_some(output.run.agent_task_id.as_deref())
        .flatten()
        .and_then(|task_id| live_agent_summary(&agent_root, task_id).ok().flatten());

    if parsed.json {
        let value = logs_json(&output, live.as_deref(), show_stdout, show_stderr);
        ctx.write_stdout(&serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }

    if let Some(text) = &live {
        ctx.write_stdout(
            "--- reconstructed from the agent store (no output was recorded for this run) ---",
        )?;
        // `text` (an agent summary) manages its own line breaks and already
        // ends in one - stripped here for the same reason `stream_section`
        // strips a stream's own trailing newline: `write_stdout` supplies
        // exactly one via `writeln!`, so leaving this one in doubles it.
        ctx.write_stdout(text.trim_end_matches('\n'))?;
        if show_stderr {
            // Through `stream_section`, not a hand-rolled `if !is_empty()`:
            // `live` is only ever computed when `show_stdout` already holds
            // (see above), so exactly as in the ordinary `render_run_output`
            // path both streams are selected here whenever `show_stderr`
            // does - meaning `stderr` must get the same labelled, "(empty)"
            // on nothing, treatment as it would there, not silently vanish
            // when this run's `stderr` also happens to be empty.
            ctx.write_stdout(&stream_section("stderr", &output.stderr, true))?;
        }
        return Ok(());
    }

    ctx.write_stdout(&render_run_output(&output, show_stdout, show_stderr))?;
    Ok(())
}

/// `Ok(None)` covers both "no agent job ever attached a task id here" and
/// "the task no longer exists in the agent store" (e.g. `agent delete` ran
/// since) - neither is an error, there is just nothing left to reconstruct.
/// Takes the agent store's root explicitly, rather than reading
/// `config_paths::agent_state_dir()` itself, so a test can point it at a
/// temporary directory instead of the real one.
///
/// `pub(in crate::cron)`, not private: `cron_manage(action=logs)`
/// (`dsh/src/cron/tool.rs`) shares this rather than returning an
/// uninformative empty `stdout` for the exact run its own primary consumer
/// - an agent inspecting its own job - most needs to see reconstructed.
pub(in crate::cron) fn live_agent_summary(
    agent_root: &std::path::Path,
    task_id: &str,
) -> Result<Option<String>> {
    let task_store = SqliteTaskStore::open(agent_root)?;
    match task_store.load(task_id) {
        Ok(task) => {
            let events = task_store.events(task_id).unwrap_or_default();
            Ok(Some(summary::task_summary(&task, &events)))
        }
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::agent::{AgentTask, TaskGrant, TaskStatus};
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
        let value = logs_json(&run_output("out", "err"), None, true, true);
        assert_eq!(value["stdout"], serde_json::json!("out"));
        assert_eq!(value["stderr"], serde_json::json!("err"));
    }

    /// The bug this guards against: `--json --stdout` used to still embed a
    /// populated `stderr` field, unlike the non-JSON path, which does honor
    /// `--stdout`/`--stderr`.
    #[test]
    fn json_with_stdout_only_omits_stderr() {
        let value = logs_json(&run_output("out", "err"), None, true, false);
        assert_eq!(value["stdout"], serde_json::json!("out"));
        assert!(value.get("stderr").is_none(), "{value}");
    }

    #[test]
    fn json_with_stderr_only_omits_stdout() {
        let value = logs_json(&run_output("out", "err"), None, false, true);
        assert!(value.get("stdout").is_none(), "{value}");
        assert_eq!(value["stderr"], serde_json::json!("err"));
    }

    #[test]
    fn json_reports_whether_the_stdout_was_reconstructed() {
        let value = logs_json(&run_output("", "err"), Some("live summary"), true, true);
        assert_eq!(value["stdout"], serde_json::json!("live summary"));
        assert_eq!(value["reconstructed"], serde_json::json!(true));
    }

    fn task(id: &str) -> AgentTask {
        AgentTask {
            id: id.to_string(),
            goal: "do it".to_string(),
            root: "/tmp".into(),
            status: TaskStatus::Completed,
            grant: TaskGrant::default(),
            criteria: vec![],
            plan: vec![],
            progress: String::new(),
            token_budget: 100,
            tokens_used: 10,
            time_budget_ms: 1_000,
            elapsed_ms: 10,
            stop_reason: None,
            checkpoint: None,
            pending_operation: None,
            created_at: 0,
        }
    }

    // `SqliteTaskStore::open` insists its root is private (0700); a fresh
    // `tempfile::tempdir()` inherits the process umask instead, so tests -
    // like `dsh/src/cron/store/tests.rs`'s own `root()` - point it at a path
    // *inside* the temporary directory rather than at the directory itself.
    fn agent_root(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("agent")
    }

    #[test]
    fn live_agent_summary_reconstructs_from_the_agent_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_store = SqliteTaskStore::open(&agent_root(&dir)).expect("open");
        task_store.save(&task("task-1"), None).expect("save");

        let text = live_agent_summary(&agent_root(&dir), "task-1")
            .expect("no io error")
            .expect("task found");
        assert!(text.contains("goal: do it"), "{text}");
    }

    /// Covers both "no such task" and "no agent store at all yet" (a fresh
    /// job whose first run has not even started) - neither is an error.
    #[test]
    fn live_agent_summary_is_none_for_an_unknown_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            live_agent_summary(&agent_root(&dir), "nope").expect("no io error"),
            None
        );
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
