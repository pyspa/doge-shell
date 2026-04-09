//! History command handler.

use crate::history::{HistoryQuery, HistoryScope, HistoryStatusFilter};
use crate::shell::Shell;
use anyhow::Result;
use chrono::{Local, TimeZone};
use dsh_types::Context;

/// Execute the `history` builtin command.
///
/// Displays the command history.
pub fn execute(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    if let Some(ref mut history) = shell.cmd_history {
        let mut history = history.lock();
        let options = HistoryOptions::parse(&argv[1..]);

        if options.help {
            print_help(ctx)?;
            history.reset_index();
            return Ok(());
        }

        let current_cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let query = HistoryQuery {
            text: options.query.clone(),
            scope: options.scope,
            status: options.status,
            min_duration_ms: options.min_duration_ms,
            limit: Some(options.limit),
            current_cwd: current_cwd.clone(),
            current_project: crate::history::get_current_context(),
            current_session_id: Some(shell.session_id.clone()),
        };

        for item in history.search_entries(&query) {
            if options.verbose || options.has_filters() {
                ctx.write_stdout(&format_entry(&item))?;
            } else {
                ctx.write_stdout(&item.entry)?;
            }
        }
        history.reset_index();
    }
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

fn print_help(ctx: &Context) -> Result<()> {
    ctx.write_stdout(
        "history [query] [--scope global|session|cwd|project] [--status any|success|failure] [--slow <ms>] [--limit <n>] [--verbose]",
    )?;
    Ok(())
}
