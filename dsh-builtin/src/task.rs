use super::ShellProxy;
use crate::project_context;
use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use regex::Regex;
use serde::Serialize;
use skim::prelude::*;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tabled::{Table, Tabled};

pub fn description() -> &'static str {
    "Run project-specific tasks (npm, cargo, make, deno, just, etc.)"
}

#[derive(Debug, Clone, Serialize, Tabled)]
struct Task {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Task")]
    name: String,
    #[tabled(rename = "Command")]
    command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskInfo {
    pub source: String,
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDiscoverySummary {
    pub tasks: Vec<TaskInfo>,
    pub deferred_sources: Vec<String>,
}

impl From<TaskInfo> for Task {
    fn from(info: TaskInfo) -> Self {
        Task {
            source: info.source,
            name: info.name,
            command: info.command,
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
        std::borrow::Cow::Borrowed(&self.command)
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
            let _ = ctx.write_stderr(&format!("Failed to detect tasks: {}\n", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    if tasks.is_empty() {
        let _ = ctx.write_stdout("No tasks detected in current directory.\n");
        return ExitStatus::ExitedWith(0);
    }

    if opts.list || opts.json {
        let filtered =
            filtered_tasks_for_request(&tasks, opts.source.as_deref(), opts.target.as_deref());
        return print_tasks(ctx, &filtered, opts.json);
    }

    if let Some(target_name) = opts.target.as_deref() {
        match select_task(&tasks, opts.source.as_deref(), target_name) {
            TaskSelection::Selected(task) => return execute_task(ctx, task, proxy),
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
            let _ = ctx.write_stderr(&format!("Error: {}\n", e));
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
        let _ = ctx.write_stdout(&format!("Running: {}\n", command));

        match crate::dispatch_shell_command(ctx, proxy, command) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("Execution failed: {}\n", e));
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
}

fn parse_options(args: &[String]) -> std::result::Result<TaskOptions, String> {
    let mut opts = TaskOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
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
        "Usage: task [--list|--json] [--source <source>] [<task>|<source>:<task>]\n",
        "\n",
        "Run or list project-specific tasks detected from package.json, Cargo.toml, Makefile, Justfile, mise, Taskfile, turbo, nx, and deno.\n",
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

fn execute_task(ctx: &Context, task: &Task, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let _ = ctx.write_stdout(&format!(
        "Running [{}] {} -> {}\n",
        task.source, task.name, task.command
    ));
    match crate::dispatch_shell_command(ctx, proxy, task.command.clone()) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("Execution failed: {}\n", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn detect_tasks(proxy: &dyn ShellProxy) -> Result<Vec<Task>> {
    let current_dir = proxy.get_current_dir()?;
    let tasks = list_tasks_in_dir(&current_dir)?;
    Ok(tasks.into_iter().map(Task::from).collect())
}

pub fn list_tasks_in_dir(current_dir: &Path) -> Result<Vec<TaskInfo>> {
    Ok(detect_tasks_in_dir(current_dir, TaskDetectionMode::Full, None)?.tasks)
}

pub fn list_tasks_in_dir_for_sources(
    current_dir: &Path,
    sources: &[&str],
) -> Result<Vec<TaskInfo>> {
    Ok(detect_tasks_in_dir(current_dir, TaskDetectionMode::Full, Some(sources))?.tasks)
}

pub fn summarize_tasks_in_dir_metadata_only(current_dir: &Path) -> Result<TaskDiscoverySummary> {
    detect_tasks_in_dir(current_dir, TaskDetectionMode::MetadataOnly, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskDetectionMode {
    Full,
    MetadataOnly,
}

fn detect_tasks_in_dir(
    current_dir: &Path,
    mode: TaskDetectionMode,
    source_filter: Option<&[&str]>,
) -> Result<TaskDiscoverySummary> {
    let project_tasks = if any_source_enabled(source_filter, &["mise", "taskfile", "turbo", "nx"]) {
        project_context::detect_task_names_in_dir(current_dir)?
            .into_iter()
            .filter(|task| source_enabled(source_filter, &task.source))
            .map(|task| TaskInfo {
                source: task.source,
                name: task.name,
                command: task.command,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let project = project_context::resolve_project_context(current_dir);
    let current_dir = project.project_root.as_path();
    let mut tasks = Vec::new();
    let mut deferred_sources = Vec::new();

    tasks.extend(project_tasks);

    // 1. package.json (npm, yarn, pnpm, bun)
    if any_source_enabled(source_filter, &["npm", "pnpm", "yarn", "bun"])
        && let Ok(content) = fs::read_to_string(current_dir.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(scripts) = json.get("scripts").and_then(|s| s.as_object())
    {
        let manager = detect_js_manager(current_dir);
        if source_enabled(source_filter, &manager) {
            for name in scripts.keys() {
                tasks.push(TaskInfo {
                    source: manager.clone(),
                    name: name.clone(),
                    // e.g. "npm run build"
                    command: format!("{} run {}", manager, name),
                });
            }
        }
    }

    // 2. Cargo.toml
    if source_enabled(source_filter, "cargo") && current_dir.join("Cargo.toml").exists() {
        // Standard cargo commands
        for cmd in ["build", "run", "test", "check", "clippy", "fmt", "doc"] {
            tasks.push(TaskInfo {
                source: "cargo".to_string(),
                name: cmd.to_string(),
                command: format!("cargo {}", cmd),
            });
        }
    }

    // 3. Makefile
    if source_enabled(source_filter, "make")
        && (current_dir.join("Makefile").exists() || current_dir.join("makefile").exists())
    {
        match mode {
            TaskDetectionMode::Full => {
                // Use make -pRrq : to list targets. This can evaluate Makefile constructs,
                // so passive diagnostics must use MetadataOnly mode instead.
                if let Ok(output) = Command::new("make")
                    .current_dir(current_dir)
                    .args(["-pRrq", ":"])
                    .output()
                {
                    let content = String::from_utf8_lossy(&output.stdout);
                    for line in content.lines() {
                        if let Some(target) = line.strip_suffix(':')
                            && !target.starts_with(['.', '#', '%'])
                            && !target.contains('%')
                            && !target.contains(' ')
                        {
                            tasks.push(TaskInfo {
                                source: "make".to_string(),
                                name: target.to_string(),
                                command: format!("make {}", target),
                            });
                        }
                    }
                }
            }
            TaskDetectionMode::MetadataOnly => deferred_sources.push("make".to_string()),
        }
    }

    // 4. deno.json / deno.jsonc
    let deno_json = current_dir.join("deno.json");
    let deno_jsonc = current_dir.join("deno.jsonc");
    let deno_path = if deno_json.exists() {
        Some(deno_json)
    } else if deno_jsonc.exists() {
        Some(deno_jsonc)
    } else {
        None
    };

    if let Some(path) = deno_path
        && source_enabled(source_filter, "deno")
        && let Ok(content) = fs::read_to_string(&path)
    {
        let clean_content = remove_jsonc_comments(&content);
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&clean_content)
            && let Some(task_obj) = json.get("tasks").and_then(|t| t.as_object())
        {
            for (name, _) in task_obj {
                tasks.push(TaskInfo {
                    source: "deno".to_string(),
                    name: name.clone(),
                    command: format!("deno task {}", name),
                });
            }
        }
    }

    // 5. Justfile
    let justfile_exists = ["Justfile", "justfile", ".justfile"]
        .iter()
        .any(|f| current_dir.join(f).exists());
    if source_enabled(source_filter, "just") && justfile_exists {
        match mode {
            TaskDetectionMode::Full => {
                // Try `just --summary`. Keep this out of passive diagnostics because
                // justfiles may invoke shell during evaluation.
                if let Ok(output) = Command::new("just")
                    .current_dir(current_dir)
                    .arg("--summary")
                    .output()
                {
                    let text = String::from_utf8_lossy(&output.stdout);
                    for name in text.split_whitespace() {
                        tasks.push(TaskInfo {
                            source: "just".to_string(),
                            name: name.to_string(),
                            command: format!("just {}", name),
                        });
                    }
                }
            }
            TaskDetectionMode::MetadataOnly => deferred_sources.push("just".to_string()),
        }
    }

    Ok(TaskDiscoverySummary {
        tasks: dedup_task_infos(tasks),
        deferred_sources: dedup_strings(deferred_sources),
    })
}

fn source_enabled(source_filter: Option<&[&str]>, source: &str) -> bool {
    source_filter.is_none_or(|sources| sources.contains(&source))
}

fn any_source_enabled(source_filter: Option<&[&str]>, sources: &[&str]) -> bool {
    source_filter.is_none_or(|filter| sources.iter().any(|source| filter.contains(source)))
}

fn detect_js_manager(path: &Path) -> String {
    if path.join("bun.lockb").exists() {
        "bun".to_string()
    } else if path.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else if path.join("yarn.lock").exists() {
        "yarn".to_string()
    } else {
        "npm".to_string()
    }
}

use std::sync::LazyLock;

static JSONC_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)//[^\n]*|/\*.*?\*/").expect("Invalid JSONC comment regex"));

fn remove_jsonc_comments(json: &str) -> String {
    JSONC_COMMENT_RE.replace_all(json, "").to_string()
}

fn dedup_task_infos(tasks: Vec<TaskInfo>) -> Vec<TaskInfo> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for task in tasks {
        let key = (task.source.clone(), task.name.clone(), task.command.clone());
        if seen.insert(key) {
            deduped.push(task);
        }
    }
    deduped
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::tempdir;

    struct MockShellProxy {
        current_dir: PathBuf,
        dispatched: Option<(String, Vec<String>)>,
    }

    impl MockShellProxy {
        fn new(current_dir: PathBuf) -> Self {
            Self {
                current_dir,
                dispatched: None,
            }
        }
    }

    impl ShellProxy for MockShellProxy {
        fn exit_shell(&mut self) {}

        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }

        fn get_git_branch(&self) -> Option<String> {
            None
        }

        fn get_job_count(&self) -> usize {
            0
        }

        fn dispatch(&mut self, _ctx: &Context, cmd: &str, argv: Vec<String>) -> anyhow::Result<()> {
            self.dispatched = Some((cmd.to_string(), argv));
            Ok(())
        }

        fn save_path_history(&mut self, _path: &str) {}

        fn changepwd(&mut self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn insert_path(&mut self, _index: usize, _path: &str) {}

        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }

        fn set_var(&mut self, _key: String, _value: String) {}

        fn set_env_var(&mut self, _key: String, _value: String) {}

        fn unset_env_var(&mut self, _key: &str) {}

        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }

        fn set_alias(&mut self, _name: String, _command: String) {}

        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }

        fn add_abbr(&mut self, _name: String, _expansion: String) {}

        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }

        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }

        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }

        fn list_mcp_servers(&mut self) -> Vec<dsh_types::mcp::McpServerConfig> {
            Vec::new()
        }

        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }

        fn list_exported_vars(&self) -> Vec<(String, String)> {
            Vec::new()
        }

        fn export_var(&mut self, _key: &str) -> bool {
            false
        }

        fn set_and_export_var(&mut self, _key: String, _value: String) {}

        fn get_current_dir(&self) -> anyhow::Result<PathBuf> {
            Ok(self.current_dir.clone())
        }

        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn test_detect_package_json() {
        let dir = tempdir().unwrap();
        let package_json = r#"{
            "scripts": {
                "start": "node index.js",
                "test": "jest"
            }
        }"#;
        let mut file = File::create(dir.path().join("package.json")).unwrap();
        file.write_all(package_json.as_bytes()).unwrap();

        let tasks = list_tasks_in_dir(dir.path()).unwrap();
        let start_task = tasks.iter().find(|t| t.name == "start").unwrap();
        assert_eq!(start_task.source, "npm");
        assert_eq!(start_task.command, "npm run start");

        let test_task = tasks.iter().find(|t| t.name == "test").unwrap();
        assert_eq!(test_task.source, "npm");
    }

    #[test]
    fn test_detect_cargo_toml() {
        let dir = tempdir().unwrap();
        File::create(dir.path().join("Cargo.toml")).unwrap();

        let tasks = list_tasks_in_dir(dir.path()).unwrap();
        assert!(
            tasks
                .iter()
                .any(|t| t.name == "build" && t.source == "cargo")
        );
        assert!(
            tasks
                .iter()
                .any(|t| t.name == "check" && t.source == "cargo")
        );
    }

    #[test]
    fn test_detect_yarn() {
        let dir = tempdir().unwrap();
        let package_json = r#"{ "scripts": { "build": "echo build" } }"#;
        File::create(dir.path().join("package.json"))
            .unwrap()
            .write_all(package_json.as_bytes())
            .unwrap();
        File::create(dir.path().join("yarn.lock")).unwrap();

        let tasks = list_tasks_in_dir(dir.path()).unwrap();
        let task = tasks.first().unwrap();
        assert_eq!(task.source, "yarn");
        assert_eq!(task.command, "yarn run build");
    }

    #[test]
    fn test_detect_tasks_from_project_root_when_called_from_subdir() {
        let dir = tempdir().unwrap();
        let package_json = r#"{ "scripts": { "build": "echo build" } }"#;
        File::create(dir.path().join("package.json"))
            .unwrap()
            .write_all(package_json.as_bytes())
            .unwrap();
        File::create(dir.path().join("mise.toml"))
            .unwrap()
            .write_all(b"[tasks.dev]\nrun = 'npm run dev'\n")
            .unwrap();
        let nested = dir.path().join("src").join("nested");
        fs::create_dir_all(&nested).unwrap();

        let tasks = list_tasks_in_dir(&nested).unwrap();
        assert!(
            tasks
                .iter()
                .any(|task| task.name == "build" && task.source == "npm")
        );
        assert!(
            tasks
                .iter()
                .any(|task| task.name == "dev" && task.source == "mise")
        );
    }

    #[test]
    fn source_scoped_task_detection_does_not_execute_makefile_for_npm() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("should-not-exist");
        File::create(dir.path().join("package.json"))
            .unwrap()
            .write_all(br#"{ "scripts": { "build": "echo build" } }"#)
            .unwrap();
        File::create(dir.path().join("Makefile"))
            .unwrap()
            .write_all(
                format!("$(shell touch {})\nall:\n\t@echo all\n", marker.display()).as_bytes(),
            )
            .unwrap();

        let tasks = list_tasks_in_dir_for_sources(dir.path(), &["npm"]).unwrap();
        assert!(
            tasks
                .iter()
                .any(|task| task.source == "npm" && task.name == "build")
        );
        assert!(
            !marker.exists(),
            "npm-scoped task detection must not invoke make"
        );
    }

    #[test]
    fn metadata_summary_does_not_execute_makefile() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("should-not-exist");
        File::create(dir.path().join("Makefile"))
            .unwrap()
            .write_all(
                format!("$(shell touch {})\nall:\n\t@echo all\n", marker.display()).as_bytes(),
            )
            .unwrap();

        let summary = summarize_tasks_in_dir_metadata_only(dir.path()).unwrap();
        assert!(summary.tasks.is_empty());
        assert_eq!(summary.deferred_sources, vec!["make".to_string()]);
        assert!(
            !marker.exists(),
            "metadata-only task summary must not invoke make"
        );
    }

    #[test]
    fn parse_options_preserves_task_name_literals() {
        let args = vec![
            "--source".to_string(),
            "cargo".to_string(),
            "build".to_string(),
        ];
        let opts = parse_options(&args).unwrap();
        assert_eq!(opts.source.as_deref(), Some("cargo"));
        assert_eq!(opts.target.as_deref(), Some("build"));

        let args = vec!["npm:test".to_string()];
        let opts = parse_options(&args).unwrap();
        assert_eq!(opts.source.as_deref(), None);
        assert_eq!(opts.target.as_deref(), Some("npm:test"));
    }

    #[test]
    fn parse_options_allows_colon_task_names_with_source_filter() {
        let args = vec![
            "--source".to_string(),
            "npm".to_string(),
            "lint:fix".to_string(),
        ];
        let opts = parse_options(&args).unwrap();
        assert_eq!(opts.source.as_deref(), Some("npm"));
        assert_eq!(opts.target.as_deref(), Some("lint:fix"));
    }

    #[test]
    fn select_task_reports_ambiguous_names() {
        let tasks = vec![
            Task {
                source: "cargo".to_string(),
                name: "test".to_string(),
                command: "cargo test".to_string(),
            },
            Task {
                source: "npm".to_string(),
                name: "test".to_string(),
                command: "npm run test".to_string(),
            },
        ];

        match select_task(&tasks, None, "test") {
            TaskSelection::Ambiguous { matches, .. } => assert_eq!(matches.len(), 2),
            _ => panic!("expected ambiguous task selection"),
        }

        match select_task(&tasks, Some("cargo"), "test") {
            TaskSelection::Selected(task) => assert_eq!(task.command, "cargo test"),
            _ => panic!("expected source-qualified task selection"),
        }
    }

    #[test]
    fn select_task_supports_qualified_source_without_breaking_colon_task_names() {
        let tasks = vec![
            Task {
                source: "cargo".to_string(),
                name: "build".to_string(),
                command: "cargo build".to_string(),
            },
            Task {
                source: "npm".to_string(),
                name: "lint:fix".to_string(),
                command: "npm run lint:fix".to_string(),
            },
        ];

        match select_task(&tasks, None, "cargo:build") {
            TaskSelection::Selected(task) => assert_eq!(task.command, "cargo build"),
            _ => panic!("expected known source-qualified task selection"),
        }

        match select_task(&tasks, None, "lint:fix") {
            TaskSelection::Selected(task) => assert_eq!(task.command, "npm run lint:fix"),
            _ => panic!("expected colon-containing task name to remain literal"),
        }

        match select_task(&tasks, Some("npm"), "lint:fix") {
            TaskSelection::Selected(task) => assert_eq!(task.command, "npm run lint:fix"),
            _ => panic!("expected source filter with colon-containing task name"),
        }
    }

    #[test]
    fn filtered_tasks_supports_source_and_target_filters() {
        let tasks = vec![
            Task {
                source: "cargo".to_string(),
                name: "build".to_string(),
                command: "cargo build".to_string(),
            },
            Task {
                source: "cargo".to_string(),
                name: "test".to_string(),
                command: "cargo test".to_string(),
            },
            Task {
                source: "npm".to_string(),
                name: "test".to_string(),
                command: "npm run test".to_string(),
            },
        ];

        let filtered = filtered_tasks(&tasks, Some("cargo"), Some("test"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command, "cargo test");
    }

    #[test]
    fn filtered_tasks_for_request_supports_known_source_qualifier() {
        let tasks = vec![
            Task {
                source: "cargo".to_string(),
                name: "build".to_string(),
                command: "cargo build".to_string(),
            },
            Task {
                source: "npm".to_string(),
                name: "lint:fix".to_string(),
                command: "npm run lint:fix".to_string(),
            },
        ];

        let filtered = filtered_tasks_for_request(&tasks, None, Some("cargo:build"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command, "cargo build");

        let filtered = filtered_tasks_for_request(&tasks, None, Some("lint:fix"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command, "npm run lint:fix");
    }

    #[test]
    fn command_dispatches_selected_task_through_shell_without_duplicate_sh() {
        let dir = tempdir().unwrap();
        File::create(dir.path().join("Cargo.toml")).unwrap();
        let mut proxy = MockShellProxy::new(dir.path().to_path_buf());
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);

        let status = command(
            &ctx,
            vec!["task".to_string(), "build".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(
            proxy.dispatched,
            Some((
                "sh".to_string(),
                vec!["-c".to_string(), "cargo build".to_string()]
            ))
        );
    }
}
