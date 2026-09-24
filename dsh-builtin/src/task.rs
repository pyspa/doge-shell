use super::ShellProxy;
use crate::project_context;
use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use regex::Regex;
use serde::Serialize;
use skim::prelude::*;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tabled::{Table, Tabled};
use wait_timeout::ChildExt;

mod detect;
mod providers;
pub mod runtime;
use detect::detect_tasks_in_dir;
#[cfg(test)]
use detect::parse_gradle_task_names;
use providers::discover_provider_tasks;
#[cfg(test)]
use providers::{parse_mise_tasks_json, parse_turbo_tasks_json};
pub use runtime::{TaskDiscoveryRuntime, TaskDiscoverySignature, discovery_signature};

pub fn description() -> &'static str {
    "Run project-specific tasks (npm, cargo, gradle, make, deno, just, etc.)"
}

#[derive(Debug, Clone, Serialize)]
struct Task {
    id: String,
    source: String,
    name: String,
    command: String,
    description: Option<String>,
    cwd: String,
}

impl Task {
    #[cfg(test)]
    fn test(source: &str, name: &str, command: &str) -> Self {
        TaskInfo::new(source, name, command, "/tmp").into()
    }
}

impl Tabled for Task {
    const LENGTH: usize = 4;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(self.id.as_str()),
            Cow::Borrowed(self.name.as_str()),
            Cow::Borrowed(self.command.as_str()),
            Cow::Borrowed(self.cwd.as_str()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![
            Cow::Borrowed("ID"),
            Cow::Borrowed("Task"),
            Cow::Borrowed("Command"),
            Cow::Borrowed("Cwd"),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub source: String,
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub cwd: String,
}

impl TaskInfo {
    pub fn new(
        source: impl Into<String>,
        name: impl Into<String>,
        command: impl Into<String>,
        cwd: impl Into<String>,
    ) -> Self {
        let source = source.into();
        let name = name.into();
        Self {
            id: format!("{source}:{name}"),
            source,
            name,
            command: command.into(),
            description: None,
            cwd: cwd.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDiscoverySummary {
    pub tasks: Vec<TaskInfo>,
    pub deferred_sources: Vec<String>,
}

impl From<TaskInfo> for Task {
    fn from(info: TaskInfo) -> Self {
        Task {
            id: info.id,
            source: info.source,
            name: info.name,
            command: info.command,
            description: info.description,
            cwd: info.cwd,
        }
    }
}

impl SkimItem for Task {
    fn text(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(format!(
            "[{}] {}  ({})",
            self.source, self.name, self.command
        ))
    }

    fn output(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(task_execution_command(self, &[]))
    }
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let opts = match parse_options(&argv[1..]) {
        Ok(opts) => opts,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("task: {err}"));
            let _ = ctx.write_stderr(help_text());
            return ExitStatus::ExitedWith(1);
        }
    };

    if opts.help {
        let _ = ctx.write_stdout(help_text());
        return ExitStatus::ExitedWith(0);
    }

    let tasks = match detect_tasks(proxy) {
        Ok(t) => t,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("Failed to detect tasks: {}", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    if tasks.is_empty() {
        let _ = ctx.write_stdout(if opts.json {
            "[]"
        } else {
            "No tasks detected in current directory.\n"
        });
        return ExitStatus::ExitedWith(0);
    }

    if opts.list || opts.json {
        let filtered =
            filtered_tasks_for_request(&tasks, opts.source.as_deref(), opts.target.as_deref());
        return print_tasks(ctx, &filtered, opts.json);
    }

    if let Some(target_name) = opts.target.as_deref() {
        match select_task(&tasks, opts.source.as_deref(), target_name) {
            TaskSelection::Selected(task) => {
                return execute_task(ctx, task, &opts.forward_args, proxy);
            }
            TaskSelection::NotFound { target, source } => {
                if let Some(source) = source {
                    let _ = ctx
                        .write_stderr(&format!("Task '{target}' not found for source '{source}'."));
                } else {
                    let _ = ctx.write_stderr(&format!("Task '{target}' not found."));
                }
                return ExitStatus::ExitedWith(1);
            }
            TaskSelection::Ambiguous { target, matches } => {
                let _ = ctx.write_stderr(&format!(
                    "Task '{target}' is ambiguous. Use one of these qualified names:"
                ));
                for task in matches {
                    let _ = ctx.write_stderr(&format!(
                        "  task {}:{}    # {}",
                        task.source, task.name, task.command
                    ));
                }
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    let filtered = filtered_tasks(&tasks, opts.source.as_deref(), None);
    if filtered.is_empty() {
        if let Some(source) = opts.source {
            let _ = ctx.write_stdout(&format!("No tasks detected for source '{source}'."));
        } else {
            let _ = ctx.write_stdout("No tasks detected in current directory.");
        }
        return ExitStatus::ExitedWith(0);
    }

    if !ctx.interactive {
        let _ = ctx.write_stdout(
            "Non-interactive mode; listing tasks. Use `task <source>:<name>` to run one.",
        );
        return print_tasks(ctx, &filtered, false);
    }

    // Interactive mode
    let options = SkimOptionsBuilder::default()
        .prompt("Task> ".to_string())
        .height("40%".to_string())
        .multi(false)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e));

    let options = match options {
        Ok(opt) => opt,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("Error: {}", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
    for task in filtered.into_iter().cloned() {
        let _ = tx.send(vec![Arc::new(task)]);
    }
    drop(tx);

    let selected = crate::skim_runner::run_skim_with(options, Some(rx))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    if let Some(item) = selected.first() {
        // Downcast back to Task - but SkimItem logic handles output()
        let command = item.output().to_string();
        // Print what we run
        let _ = ctx.write_stdout(&format!("Running: {}", command));

        match crate::dispatch_shell_command(ctx, proxy, command) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("Execution failed: {}", e));
                ExitStatus::ExitedWith(1)
            }
        }
    } else {
        ExitStatus::ExitedWith(0)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TaskOptions {
    list: bool,
    json: bool,
    help: bool,
    source: Option<String>,
    target: Option<String>,
    forward_args: Vec<String>,
}

fn parse_options(args: &[String]) -> std::result::Result<TaskOptions, String> {
    let mut opts = TaskOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                opts.forward_args = args[index + 1..].to_vec();
                break;
            }
            "-h" | "--help" | "help" => {
                opts.help = true;
            }
            "-l" | "--list" | "list" => {
                opts.list = true;
            }
            "--json" => {
                opts.json = true;
                opts.list = true;
            }
            "-s" | "--source" => {
                index += 1;
                let Some(source) = args.get(index) else {
                    return Err("--source requires a source name".to_string());
                };
                opts.source = Some(source.clone());
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option: {value}"));
            }
            value => {
                if opts.target.is_some() {
                    return Err(format!("unexpected argument: {value}"));
                }
                opts.target = Some(value.to_string());
            }
        }
        index += 1;
    }

    Ok(opts)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: task [--list|--json] [--source <source>] [<task>|<source>:<task>] [-- <args>]\n",
        "\n",
        "Run or list project-specific tasks detected from package.json, Cargo.toml, Gradle, Makefile, Justfile, mise, Taskfile, turbo, nx, and deno.\n",
        "\n",
        "Options:\n",
        "  -l, --list            List detected tasks\n",
        "      --json            List detected tasks as JSON\n",
        "  -s, --source <source> Filter by task source\n",
        "  -h, --help            Show this help\n",
        "\n",
        "Examples:\n",
        "  task\n",
        "  task --list\n",
        "  task --source cargo build\n",
        "  task cargo:build\n",
        "  task npm:test -- --watch\n",
    )
}

fn split_qualified_task(target: &str) -> Option<(&str, &str)> {
    let (source, name) = target.split_once(':')?;
    (!source.is_empty() && !name.is_empty()).then_some((source, name))
}

fn split_qualified_task_for_known_source<'a>(
    tasks: &[Task],
    source: Option<&'a str>,
    target: &'a str,
) -> (Option<&'a str>, &'a str) {
    if source.is_none()
        && let Some((candidate_source, name)) = split_qualified_task(target)
        && tasks.iter().any(|task| task.source == candidate_source)
    {
        return (Some(candidate_source), name);
    }

    (source, target)
}

fn filtered_tasks<'a>(
    tasks: &'a [Task],
    source: Option<&str>,
    target: Option<&str>,
) -> Vec<&'a Task> {
    tasks
        .iter()
        .filter(|task| source.is_none_or(|source| task.source == source))
        .filter(|task| target.is_none_or(|target| task.name == target))
        .collect()
}

fn filtered_tasks_for_request<'a>(
    tasks: &'a [Task],
    source: Option<&str>,
    target: Option<&str>,
) -> Vec<&'a Task> {
    if let Some(target) = target {
        let (source, target) = split_qualified_task_for_known_source(tasks, source, target);
        filtered_tasks(tasks, source, Some(target))
    } else {
        filtered_tasks(tasks, source, None)
    }
}

enum TaskSelection<'a> {
    Selected(&'a Task),
    Ambiguous {
        target: String,
        matches: Vec<&'a Task>,
    },
    NotFound {
        target: String,
        source: Option<String>,
    },
}

fn select_task<'a>(tasks: &'a [Task], source: Option<&str>, target: &str) -> TaskSelection<'a> {
    let (source, target) = split_qualified_task_for_known_source(tasks, source, target);
    let matched = filtered_tasks(tasks, source, Some(target));
    match matched.len() {
        0 => TaskSelection::NotFound {
            target: target.to_string(),
            source: source.map(str::to_string),
        },
        1 => TaskSelection::Selected(matched[0]),
        _ => TaskSelection::Ambiguous {
            target: target.to_string(),
            matches: matched,
        },
    }
}

fn print_tasks(ctx: &Context, tasks: &[&Task], json: bool) -> ExitStatus {
    if json {
        let rows: Vec<Task> = tasks.iter().map(|task| (*task).clone()).collect();
        match serde_json::to_string_pretty(&rows) {
            Ok(output) => {
                let _ = ctx.write_stdout(&output);
                return ExitStatus::ExitedWith(0);
            }
            Err(err) => {
                let _ = ctx.write_stderr(&format!("task: failed to serialize tasks: {err}"));
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    if tasks.is_empty() {
        let _ = ctx.write_stdout("No tasks matched.");
        return ExitStatus::ExitedWith(0);
    }

    let rows: Vec<Task> = tasks.iter().map(|task| (*task).clone()).collect();
    let _ = ctx.write_stdout(&Table::new(rows).to_string());
    ExitStatus::ExitedWith(0)
}

fn execute_task(
    ctx: &Context,
    task: &Task,
    forward_args: &[String],
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    let command = task_execution_command(task, forward_args);
    let _ = ctx.write_stdout(&format!(
        "Running [{}] {} -> {}",
        task.source, task.name, command
    ));
    match crate::dispatch_shell_command(ctx, proxy, command) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("Execution failed: {}", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn task_execution_command(task: &Task, forward_args: &[String]) -> String {
    let args = forward_args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let command = if args.is_empty() {
        task.command.clone()
    } else if task.source == "npm" {
        format!("{} -- {args}", task.command)
    } else {
        format!("{} {args}", task.command)
    };
    if task.cwd.is_empty() {
        command
    } else {
        format!("cd {} && {command}", shell_quote(&task.cwd))
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn detect_tasks(proxy: &dyn ShellProxy) -> Result<Vec<Task>> {
    // One snapshot per invocation: cwd, logical PATH, and exported child
    // environment. The lock is released here; later scans and subprocesses
    // use only the owned snapshot.
    let current_dir = proxy.get_current_dir()?;
    let runtime = TaskDiscoveryRuntime::new(
        proxy.command_search_paths(),
        proxy.child_process_environment(),
    );
    let tasks = list_tasks_in_dir(&current_dir, &runtime)?;
    Ok(tasks.into_iter().map(Task::from).collect())
}

pub fn list_tasks_in_dir(
    current_dir: &Path,
    runtime: &TaskDiscoveryRuntime,
) -> Result<Vec<TaskInfo>> {
    let project = project_context::resolve_project_context(current_dir);
    let signature = discovery_signature(&project.project_root, None, runtime);
    if let Some(tasks) = TASK_CACHE
        .lock()
        .expect("task cache poisoned")
        .get(&project.project_root)
        .filter(|entry| entry.signature == signature)
        .map(|entry| entry.tasks.clone())
    {
        return Ok(tasks);
    }
    let tasks = detect_tasks_in_dir(current_dir, TaskDetectionMode::Full, None, runtime)?.tasks;
    TASK_CACHE.lock().expect("task cache poisoned").insert(
        project.project_root,
        TaskCacheEntry {
            signature,
            tasks: tasks.clone(),
        },
    );
    Ok(tasks)
}

pub fn list_tasks_in_dir_for_sources(
    current_dir: &Path,
    sources: &[&str],
    runtime: &TaskDiscoveryRuntime,
) -> Result<Vec<TaskInfo>> {
    Ok(detect_tasks_in_dir(current_dir, TaskDetectionMode::Full, Some(sources), runtime)?.tasks)
}

pub fn summarize_tasks_in_dir_metadata_only(current_dir: &Path) -> Result<TaskDiscoverySummary> {
    // No external commands by contract: an empty runtime resolves no
    // provider executables, so only static file parsers contribute. This is
    // deliberately narrower than full discovery, where installed mise/nx/
    // turbo CLIs are preferred: passive diagnostics (doctor) must not
    // execute project code, the same reason gradle/make/just defer above.
    let runtime = TaskDiscoveryRuntime::new(Vec::new(), std::collections::HashMap::new());
    detect_tasks_in_dir(current_dir, TaskDetectionMode::MetadataOnly, None, &runtime)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskDetectionMode {
    Full,
    MetadataOnly,
}

#[derive(Clone)]
struct TaskCacheEntry {
    signature: TaskDiscoverySignature,
    tasks: Vec<TaskInfo>,
}

static TASK_CACHE: LazyLock<Mutex<std::collections::HashMap<std::path::PathBuf, TaskCacheEntry>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn command_output_with_timeout(
    executable: &Path,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    child_env: &std::collections::BTreeMap<String, String>,
) -> Result<std::process::Output> {
    let executable = executable.to_path_buf();
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let cwd = cwd.to_path_buf();
    let child_env = child_env.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(command_output_worker(
            &executable,
            &args,
            &cwd,
            timeout,
            &child_env,
        ));
    });
    receiver
        .recv_timeout(timeout + Duration::from_millis(100))
        .map_err(|_| anyhow::anyhow!("task provider timed out"))?
}

fn command_output_worker(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    child_env: &std::collections::BTreeMap<String, String>,
) -> Result<std::process::Output> {
    // Shell child environment only: `env_clear` keeps a logically unset
    // process-startup variable from being rediscovered, and an exported
    // shell variable visible to the provider.
    let mut child = Command::new(executable)
        .env_clear()
        .envs(child_env.iter())
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if child.wait_timeout(timeout)?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(anyhow::anyhow!("task provider timed out"));
    }
    Ok(child.wait_with_output()?)
}

static JSONC_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)//[^\n]*|/\*.*?\*/").expect("Invalid JSONC comment regex"));

#[cfg(test)]
mod tests;
