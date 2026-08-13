//! History command handler.

use crate::history::{CommandEvent, HistoryQuery, HistoryScope, HistoryStatusFilter};
use crate::shell::Shell;
use anyhow::Result;
use chrono::{Local, TimeZone};
use dsh_types::Context;

/// Execute the `history` builtin command.
///
/// Displays the command history.
pub fn execute(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    let Some(ref mut history) = shell.cmd_history else {
        if argv.get(1).map(String::as_str) == Some("record") {
            anyhow::bail!("persistent history is unavailable");
        }
        if argv.iter().skip(1).any(|arg| arg == "--json") {
            ctx.write_stdout("[]")?;
        }
        return Ok(());
    };
    let mut history = history.lock();
    if argv.get(1).map(String::as_str) == Some("record") {
        return record_external_event(&mut history, &shell.environment, ctx, &argv[2..]);
    }
    let options = HistoryOptions::parse(&argv[1..]);

    if options.help {
        print_help(ctx)?;
        history.reset_index();
        return Ok(());
    }

    if let Some(author) = options.author.as_deref() {
        let events = history.command_events(Some(author), options.limit)?;
        if options.json {
            ctx.write_stdout(&serde_json::to_string(&events)?)?;
        } else {
            for event in events {
                ctx.write_stdout(&format_event(&event))?;
            }
        }
        history.reset_index();
        return Ok(());
    }

    let query = HistoryQuery {
        text: options.query.clone(),
        scope: options.scope,
        status: options.status,
        min_duration_ms: options.min_duration_ms,
        limit: Some(options.limit),
        ..crate::history::query_context(shell.session_id.clone())
    };

    let entries = history.search_entries(&query);
    if options.json {
        ctx.write_stdout(&serde_json::to_string(&entries)?)?;
    } else {
        for item in entries {
            if options.verbose || options.has_filters() {
                ctx.write_stdout(&format_entry(&item))?;
            } else {
                ctx.write_stdout(&item.entry)?;
            }
        }
    }
    history.reset_index();
    Ok(())
}

#[derive(Debug, Clone)]
struct HistoryOptions {
    help: bool,
    verbose: bool,
    query: Option<String>,
    scope: HistoryScope,
    status: HistoryStatusFilter,
    min_duration_ms: Option<u64>,
    limit: usize,
    json: bool,
    author: Option<String>,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            help: false,
            verbose: false,
            query: None,
            scope: HistoryScope::Global,
            status: HistoryStatusFilter::Any,
            min_duration_ms: None,
            limit: 200,
            json: false,
            author: None,
        }
    }
}

impl HistoryOptions {
    fn parse(args: &[String]) -> Self {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-h" | "--help" => options.help = true,
                "-v" | "--verbose" => options.verbose = true,
                "--json" => options.json = true,
                "--author" => {
                    if let Some(value) = args.get(i + 1) {
                        options.author = Some(value.clone());
                        i += 1;
                    }
                }
                "-q" | "--query" => {
                    if let Some(value) = args.get(i + 1) {
                        options.query = Some(value.clone());
                        i += 1;
                    }
                }
                "-s" | "--scope" => {
                    if let Some(value) = args.get(i + 1) {
                        options.scope = parse_scope(value);
                        i += 1;
                    }
                }
                "--status" => {
                    if let Some(value) = args.get(i + 1) {
                        options.status = parse_status(value);
                        i += 1;
                    }
                }
                "--slow" => {
                    if let Some(value) = args.get(i + 1) {
                        options.min_duration_ms = value.parse::<u64>().ok();
                        i += 1;
                    }
                }
                "-n" | "--limit" => {
                    if let Some(value) = args.get(i + 1) {
                        if let Ok(limit) = value.parse::<usize>() {
                            options.limit = limit.max(1);
                        }
                        i += 1;
                    }
                }
                value => {
                    if options.query.is_none() {
                        options.query = Some(value.to_string());
                    }
                }
            }
            i += 1;
        }
        options
    }

    fn has_filters(&self) -> bool {
        self.query.is_some()
            || self.scope != HistoryScope::Global
            || self.status != HistoryStatusFilter::Any
            || self.min_duration_ms.is_some()
    }
}

fn record_external_event(
    history: &mut crate::history::History,
    environment: &std::sync::Arc<parking_lot::RwLock<crate::environment::Environment>>,
    ctx: &Context,
    args: &[String],
) -> Result<()> {
    let payload = match args {
        [flag, payload] if flag == "--json" => payload,
        [value] if value.starts_with("--json=") => value.trim_start_matches("--json="),
        _ => anyhow::bail!("Usage: history record --json '<event-json>'"),
    };
    let mut event: CommandEvent = serde_json::from_str(payload)?;
    if event.command.trim().is_empty() || event.author.trim().is_empty() {
        anyhow::bail!("history record requires non-empty command and author");
    }
    let env = environment.read();
    let Some(command) = env
        .policy_state
        .secret_manager
        .process_for_history(&event.command)
    else {
        ctx.write_stdout("{\"recorded\":false,\"reason\":\"secret-filter\"}")?;
        return Ok(());
    };
    event.command = command;
    event.output = event
        .output
        .as_deref()
        .map(|output| env.policy_state.secret_manager.redact_command(output));
    drop(env);
    history.record_external_event(event)?;
    ctx.write_stdout("{\"recorded\":true}")?;
    Ok(())
}

fn parse_scope(value: &str) -> HistoryScope {
    match value {
        "session" => HistoryScope::Session,
        "cwd" => HistoryScope::Cwd,
        "project" => HistoryScope::Project,
        _ => HistoryScope::Global,
    }
}

fn parse_status(value: &str) -> HistoryStatusFilter {
    match value {
        "success" => HistoryStatusFilter::Success,
        "failure" | "failed" => HistoryStatusFilter::Failure,
        _ => HistoryStatusFilter::Any,
    }
}

fn format_entry(entry: &crate::history::Entry) -> String {
    let timestamp = Local
        .timestamp_opt(entry.when, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-".to_string());
    let status = match entry.exit_code {
        Some(0) => "ok".to_string(),
        Some(code) => format!("err:{code}"),
        None => "-".to_string(),
    };
    let duration = entry
        .duration_ms
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "-".to_string());
    let cwd = entry.cwd.as_deref().unwrap_or("-");
    format!("{timestamp}\t{status}\t{duration}\t{cwd}\t{}", entry.entry)
}

fn format_event(event: &CommandEvent) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        event.started_at,
        event.author,
        event
            .exit_code
            .map_or_else(|| "-".to_string(), |code| code.to_string()),
        event.cwd.as_deref().unwrap_or("-"),
        event.command
    )
}

fn print_help(ctx: &Context) -> Result<()> {
    ctx.write_stdout(help_text())?;
    Ok(())
}

fn help_text() -> &'static str {
    concat!(
        "Usage: history [query] [OPTIONS]\n",
        "\n",
        "Search and filter command history.\n",
        "\n",
        "Options:\n",
        "  -q, --query <text>              Match command text explicitly\n",
        "  -s, --scope <global|session|cwd|project>\n",
        "                                 Restrict search scope\n",
        "      --status <any|success|failure>\n",
        "                                 Filter by exit status\n",
        "      --slow <ms>                Show commands with duration >= ms\n",
        "  -n, --limit <n>                Limit result count (default: 200)\n",
        "  -v, --verbose                  Show timestamp, status, duration, and cwd\n",
        "      --author <human|dsh-ai|agent|all>\n",
        "      --json                     Emit one JSON value without decoration\n",
        "  -h, --help                     Show this help message\n",
        "\n",
        "You can pass the query as the first positional argument instead of --query.\n",
        "\n",
        "Examples:\n",
        "  history cargo\n",
        "  history --status failure\n",
        "  history --scope project --slow 1000 -v\n",
        "  history --author human --json\n",
        "  history record --json '{\"command\":\"cargo test\",...}'\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_text_lists_filters_and_examples() {
        let help = help_text();
        assert!(help.contains("Usage: history"));
        assert!(help.contains("--scope"));
        assert!(help.contains("--status"));
        assert!(help.contains("--slow"));
        assert!(help.contains("--limit"));
        assert!(help.contains("--verbose"));
        assert!(help.contains("history cargo"));
        assert!(help.contains("history --status failure"));
    }

    #[test]
    fn options_parse_json_and_author_filter() {
        let options = HistoryOptions::parse(&[
            "--author".to_string(),
            "agent-x".to_string(),
            "--json".to_string(),
        ]);
        assert_eq!(options.author.as_deref(), Some("agent-x"));
        assert!(options.json);
    }

    #[test]
    fn json_without_initialized_history_is_an_empty_array() {
        let environment = crate::environment::Environment::new();
        let mut shell = crate::shell::Shell::new(environment);
        assert!(shell.cmd_history.is_none());
        let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let observer = dsh_types::observed_output::ObservedOutput::shared(4096);
        ctx.output_observer = Some(observer.clone());

        execute(
            &mut shell,
            &ctx,
            vec!["history".to_string(), "--json".to_string()],
        )
        .unwrap();

        assert_eq!(observer.lock().unwrap().snapshot().stdout, "[]\n");
    }

    #[test]
    fn external_json_record_preserves_author_and_uses_event_schema() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = crate::history::History::new();
        history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
        let environment = crate::environment::Environment::new();
        let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let observer = dsh_types::observed_output::ObservedOutput::shared(4096);
        ctx.output_observer = Some(observer);
        let payload = serde_json::json!({
            "command": "cargo test",
            "cwd": "/repo",
            "started_at": chrono::Utc::now().timestamp(),
            "duration_ms": 42,
            "exit_code": 0,
            "session": "external-session",
            "host": "external-host",
            "author": "agent-x",
            "output": "API_KEY=secret"
        })
        .to_string();
        record_external_event(
            &mut history,
            &environment,
            &ctx,
            &["--json".to_string(), payload],
        )
        .unwrap();
        let event = history
            .command_events(Some("agent-x"), 1)
            .unwrap()
            .remove(0);
        assert_eq!(event.command, "cargo test");
        assert_eq!(event.session_id.as_deref(), Some("external-session"));
        assert_eq!(event.hostname.as_deref(), Some("external-host"));
        let output = event.output.unwrap();
        assert!(output.contains("***"));
        assert!(!output.contains("secret"));
    }
}
