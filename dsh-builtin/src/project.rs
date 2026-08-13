use super::ShellProxy;
use crate::project_context;
use crate::safety_policy;
use crate::task;
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use dsh_types::{Context, ExitStatus, Project};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const PROJECTS_FILE: &str = "projects.json";

pub fn description() -> &'static str {
    "Manage projects (init, status, add, list, remove, work, activate)"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Handle 'pj' alias directly
    if let Some(cmd_name) = argv.first()
        && cmd_name == "pj"
    {
        match jump(ctx, &argv[1..], proxy) {
            Ok(_) => return ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pj error: {}", e));
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    if argv.len() < 2 {
        let _ = ctx.write_stderr(help_text());
        return ExitStatus::ExitedWith(1);
    }

    match argv[1].as_str() {
        "help" | "-h" | "--help" => {
            let _ = ctx.write_stdout(help_text());
            ExitStatus::ExitedWith(0)
        }
        "init" => match init(ctx, &argv[2..], proxy) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm init error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "status" | "st" => match status(ctx, &argv[2..], proxy) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm status error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "add" => match add(ctx, &argv[2..]) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm add error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "list" | "ls" => match list(ctx, proxy) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm list error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "remove" | "rm" => match remove(ctx, &argv[2..]) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm remove error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "work" => match work(ctx, &argv[2..], proxy) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm work error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "jump" => match jump(ctx, &argv[2..], proxy) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm jump error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        "activate" => match activate(ctx, &argv[2..], proxy) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                let _ = ctx.write_stderr(&format!("pm activate error: {}", e));
                ExitStatus::ExitedWith(1)
            }
        },
        _ => {
            let _ = ctx.write_stderr(&format!("Unknown subcommand: {}", argv[1]));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn help_text() -> &'static str {
    concat!(
        "Usage: pm <init|status|add|list|remove|work|jump|activate> [args]\n",
        "\n",
        "Subcommands:\n",
        "  init [name]          Register the current project root and show onboarding status\n",
        "  status [--json]      Show project, provider, trust, lockfile, tools, and tasks\n",
        "  add [path] [name]    Register a project path\n",
        "  list | ls            List registered projects\n",
        "  remove | rm <name>   Remove a project\n",
        "  work <name>          Switch to a project\n",
        "  jump                 Select and switch to a project interactively\n",
        "  activate [--provider auto|native|mise] [--dry-run]\n",
        "\n",
        "Aliases:\n",
        "  pj [name]            Alias for pm jump\n",
        "\n",
        "Examples:\n",
        "  pm init\n",
        "  pm status\n",
        "  pm activate\n",
        "  pm activate --dry-run\n",
    )
}

fn get_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".config").join("dsh").join(PROJECTS_FILE))
}

fn load_projects() -> Result<Vec<Project>> {
    let path = get_config_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    let projects: Vec<Project> = serde_json::from_str(&content)?;
    Ok(projects)
}

pub fn list_projects() -> Result<Vec<Project>> {
    load_projects()
}

pub fn find_project_by_path(path: &Path) -> Result<Option<Project>> {
    let projects = load_projects()?;
    let path = path.canonicalize().unwrap_or(path.to_path_buf());

    // Find the project with the longest matching path prefix
    let mut best_match: Option<Project> = None;

    for p in projects {
        // Ensure project path is absolute/canonical if possible for comparison
        // (In load_projects, we assume paths are stored canonicalized or we trust them)
        if path.starts_with(&p.path) {
            match best_match {
                None => best_match = Some(p),
                Some(ref current) => {
                    // Replace if this project path is longer (more specific)
                    if p.path.components().count() > current.path.components().count() {
                        best_match = Some(p);
                    }
                }
            }
        }
    }
    Ok(best_match)
}

fn save_projects(projects: &[Project]) -> Result<()> {
    let path = get_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(projects)?;
    fs::write(path, content)?;
    Ok(())
}

fn init(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    if args.len() > 1 {
        return Err(anyhow::anyhow!("Usage: pm init [name]"));
    }

    let current_dir = proxy.get_current_dir()?;
    let context = project_context::resolve_project_context(&current_dir);
    let root = context.project_root.clone();
    let name = args
        .first()
        .cloned()
        .unwrap_or_else(|| project_name_from_path(&root));

    let mut projects = load_projects()?;
    if let Some(existing) = projects
        .iter()
        .find(|project| same_path(&project.path, &root))
    {
        let _ = ctx.write_stdout(&format!(
            "Project '{}' is already registered at {}.",
            existing.name,
            existing.path.display()
        ));
    } else {
        if projects.iter().any(|project| project.name == name) {
            return Err(anyhow::anyhow!(
                "Project name '{}' already exists. Use `pm init <name>` with another name.",
                name
            ));
        }

        projects.push(Project::new(name.clone(), root.clone()));
        save_projects(&projects)?;
        let _ = ctx.write_stdout(&format!(
            "Project '{}' initialized at {}.",
            name,
            root.display()
        ));
    }

    print_project_status(ctx, proxy, &context, &projects);
    Ok(())
}

fn status(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    let json_output = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(anyhow::anyhow!("Usage: pm status [--json]")),
    };
    let current_dir = proxy.get_current_dir()?;
    let context = project_context::resolve_project_context(&current_dir);
    let projects = load_projects()?;
    if json_output {
        let status = build_project_status(&context, &projects);
        let _ = ctx.write_stdout(&serde_json::to_string(&status)?);
    } else {
        print_project_status(ctx, proxy, &context, &projects);
        print_provider_status(ctx, &context.project_root);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ProjectStatusJson {
    cwd: String,
    root: String,
    registered: Option<String>,
    markers: Vec<String>,
    provider: String,
    trust: String,
    lockfile: Option<String>,
    missing_tools: Vec<String>,
    dev_container: Option<String>,
    runtimes: Vec<ProjectRuntimeJson>,
}

#[derive(Debug, Serialize)]
struct ProjectRuntimeJson {
    name: String,
    source: String,
    version: Option<String>,
    path: String,
}

fn build_project_status(
    context: &project_context::ProjectContext,
    projects: &[Project],
) -> ProjectStatusJson {
    let mise = MiseStatus::detect(&context.project_root);
    ProjectStatusJson {
        cwd: context.cwd.display().to_string(),
        root: context.project_root.display().to_string(),
        registered: projects
            .iter()
            .find(|project| same_path(&project.path, &context.project_root))
            .map(|project| project.name.clone()),
        markers: context.project_markers.clone(),
        provider: if matches!(mise.trust.as_str(), "trusted" | "safe") {
            "mise".to_string()
        } else {
            "native".to_string()
        },
        trust: mise.trust,
        lockfile: mise.lockfile,
        missing_tools: mise.missing_tools,
        dev_container: detect_dev_container(&context.project_root),
        runtimes: context
            .runtimes
            .iter()
            .map(|runtime| ProjectRuntimeJson {
                name: runtime.name.clone(),
                source: runtime.source.clone(),
                version: runtime.version.clone(),
                path: runtime.path.display().to_string(),
            })
            .collect(),
    }
}

fn print_provider_status(ctx: &Context, root: &Path) {
    let mise = MiseStatus::detect(root);
    let provider = if matches!(mise.trust.as_str(), "trusted" | "safe") {
        "mise"
    } else {
        "native"
    };
    let _ = ctx.write_stdout(&format!(
        "provider {provider} trust={} lockfile={} missing_tools={}",
        mise.trust,
        mise.lockfile.as_deref().unwrap_or("none"),
        if mise.missing_tools.is_empty() {
            "none".to_string()
        } else {
            mise.missing_tools.join(",")
        }
    ));
    if let Some(path) = detect_dev_container(root) {
        let _ = ctx.write_stdout(&format!(
            "dev-container {path} (open it explicitly with your editor/container tool)"
        ));
    }
}

#[derive(Debug)]
struct MiseStatus {
    executable: Option<PathBuf>,
    trust: String,
    lockfile: Option<String>,
    missing_tools: Vec<String>,
}

impl MiseStatus {
    fn detect(root: &Path) -> Self {
        Self::detect_with_executable(root, find_executable("mise"))
    }

    fn detect_with_executable(root: &Path, executable: Option<PathBuf>) -> Self {
        let configured = root.join("mise.toml").exists() || root.join(".mise.toml").exists();
        let trust = if !configured {
            "not-configured".to_string()
        } else if let Some(mise) = executable.as_deref() {
            match mise_output(mise, root, &["trust", "--show"]) {
                Ok(output) if trust_output_is_trusted(&output) => "trusted".to_string(),
                _ if mise_config_is_safe(root) => "safe".to_string(),
                _ => "untrusted".to_string(),
            }
        } else {
            "unavailable".to_string()
        };
        let lockfile = ["mise.lock", ".mise.lock"]
            .into_iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file())
            .map(|path| path.display().to_string());
        let missing_tools = if matches!(trust.as_str(), "trusted" | "safe") {
            executable
                .as_deref()
                .and_then(|mise| mise_missing_tools(mise, root).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            executable,
            trust,
            lockfile,
            missing_tools,
        }
    }
}

fn mise_missing_tools(mise: &Path, root: &Path) -> Result<Vec<String>> {
    let output = mise_output(mise, root, &["--no-hooks", "ls", "--missing", "--json"])?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let value: JsonValue = serde_json::from_slice(&output.stdout)?;
    let mut tools = Vec::new();
    collect_missing_tool_names(&value, &mut tools);
    tools.sort();
    tools.dedup();
    Ok(tools)
}

fn mise_output(mise: &Path, root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut child = Command::new(mise)
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if child.wait_timeout(Duration::from_millis(1500))?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(anyhow::anyhow!("mise provider timed out"));
    }
    Ok(child.wait_with_output()?)
}

fn trust_output_is_trusted(output: &std::process::Output) -> bool {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .any(|line| {
            line.rsplit_once(':')
                .map(|(_, status)| status.trim() == "trusted")
                .unwrap_or_else(|| line.trim() == "trusted")
        })
}

fn mise_config_is_safe(root: &Path) -> bool {
    let configs = [root.join("mise.toml"), root.join(".mise.toml")]
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    !configs.is_empty() && configs.iter().all(|path| mise_config_file_is_safe(path))
}

fn mise_config_file_is_safe(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return false;
    };
    let Some(table) = value.as_table() else {
        return false;
    };
    table.iter().all(|(key, value)| match key.as_str() {
        "min_version" => value.is_str(),
        "tools" => value.as_table().is_some_and(|tools| {
            tools.values().all(|value| {
                value.is_str()
                    || value
                        .as_array()
                        .is_some_and(|versions| versions.iter().all(toml::Value::is_str))
            })
        }),
        "tasks" => safe_mise_task_value(value, None),
        _ => false,
    })
}

fn safe_mise_task_value(value: &toml::Value, key: Option<&str>) -> bool {
    if key == Some("tools") {
        return false;
    }
    match value {
        toml::Value::String(value) => !value.contains("{{") && !value.contains("{%"),
        toml::Value::Array(values) => values.iter().all(|value| safe_mise_task_value(value, key)),
        toml::Value::Table(table) => table
            .iter()
            .all(|(key, value)| safe_mise_task_value(value, Some(key))),
        toml::Value::Integer(_) | toml::Value::Float(_) | toml::Value::Boolean(_) => true,
        toml::Value::Datetime(_) => false,
    }
}

fn collect_missing_tool_names(value: &JsonValue, tools: &mut Vec<String>) {
    match value {
        JsonValue::Array(values) => {
            for value in values {
                collect_missing_tool_names(value, tools);
            }
        }
        JsonValue::Object(object) => {
            if let Some(name) = object.get("name").and_then(JsonValue::as_str) {
                tools.push(name.to_string());
            } else {
                for (key, value) in object {
                    if value.as_bool() == Some(true)
                        || value.as_array().is_some_and(|values| !values.is_empty())
                        || value.get("missing").and_then(JsonValue::as_bool) == Some(true)
                    {
                        tools.push(key.clone());
                    }
                }
            }
        }
        JsonValue::String(name) => tools.push(name.clone()),
        _ => {}
    }
}

fn detect_dev_container(root: &Path) -> Option<String> {
    [
        root.join(".devcontainer/devcontainer.json"),
        root.join(".devcontainer.json"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .map(|path| path.display().to_string())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("project")
        .to_string()
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn print_project_status(
    ctx: &Context,
    proxy: &dyn ShellProxy,
    context: &project_context::ProjectContext,
    projects: &[Project],
) {
    let _ = ctx.write_stdout(&format!("cwd {}", context.cwd.display()));
    let _ = ctx.write_stdout(&format!("root {}", context.project_root.display()));

    if let Some(project) = projects
        .iter()
        .find(|project| same_path(&project.path, &context.project_root))
    {
        let _ = ctx.write_stdout(&format!(
            "registered {} {}",
            project.name,
            project.path.display()
        ));
    } else {
        let _ = ctx.write_stdout("registered no (run `pm init` to add this project)");
    }

    if context.project_markers.is_empty() {
        let _ = ctx.write_stdout("markers none");
    } else {
        let _ = ctx.write_stdout(&format!("markers {}", context.project_markers.join(", ")));
    }

    if context.runtimes.is_empty() {
        let _ = ctx.write_stdout("runtimes none");
    } else {
        for runtime in &context.runtimes {
            let version = runtime.version.as_deref().unwrap_or("-");
            let _ = ctx.write_stdout(&format!(
                "runtime {} source={} version={} path={}",
                runtime.name,
                runtime.source,
                version,
                runtime.path.display()
            ));
        }
    }

    if context.activations.is_empty() {
        let _ = ctx.write_stdout("activation none");
    } else {
        for activation in &context.activations {
            let _ = ctx.write_stdout(&format!(
                "activation {} {}",
                activation.kind,
                activation.path.display()
            ));
        }
        if context.project_root.join(".envrc").exists()
            && !proxy.is_direnv_allowed(&context.project_root)
        {
            let _ = ctx.write_stdout(
                "activation envrc not-allowed; add an allow-direnv entry before trusting it",
            );
        }
        if let Ok(summary) = activation_safety_summary(&context.project_root, proxy) {
            let _ = ctx.write_stdout(&summary);
        }
        let _ = ctx.write_stdout("activation hint run `pm activate`");
    }

    match task::summarize_tasks_in_dir_metadata_only(&context.project_root) {
        Ok(summary) if summary.tasks.is_empty() && summary.deferred_sources.is_empty() => {
            let _ = ctx.write_stdout("tasks none");
        }
        Ok(summary) => {
            if !summary.tasks.is_empty() {
                let counts = task_source_counts(&summary.tasks);
                let counts = counts
                    .into_iter()
                    .map(|(source, count)| format!("{source}={count}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = ctx.write_stdout(&format!(
                    "tasks {} metadata-only ({counts})",
                    summary.tasks.len()
                ));
            }
            if !summary.deferred_sources.is_empty() {
                let _ = ctx.write_stdout(&format!(
                    "tasks dynamic-probe skipped sources={} (run `task --list` for full detection)",
                    summary.deferred_sources.join(", ")
                ));
            }
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("tasks unavailable {err}"));
        }
    }
}

fn task_source_counts(tasks: &[task::TaskInfo]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for task in tasks {
        *counts.entry(task.source.clone()).or_insert(0) += 1;
    }
    counts
}

fn add(ctx: &Context, args: &[String]) -> Result<()> {
    let path = if args.is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(&args[0]).canonicalize()?
    };

    let name = if args.len() > 1 {
        args[1].clone()
    } else {
        path.file_name()
            .context("Invalid path")?
            .to_string_lossy()
            .to_string()
    };

    let mut projects = load_projects()?;

    // Check for duplicates
    if projects.iter().any(|p| p.name == name) {
        return Err(anyhow::anyhow!("Project '{}' already exists", name));
    }
    if projects.iter().any(|p| p.path == path) {
        return Err(anyhow::anyhow!(
            "Path '{}' is already registered",
            path.display()
        ));
    }

    let project = Project::new(name.clone(), path.clone());
    projects.push(project);
    save_projects(&projects)?;

    let _ = ctx.write_stdout(&format!("Project '{}' added at {}", name, path.display()));
    Ok(())
}

fn list(ctx: &Context, _proxy: &mut dyn ShellProxy) -> Result<()> {
    let mut projects = load_projects()?;
    projects.sort_by_key(|project| std::cmp::Reverse(project.last_accessed));

    if projects.is_empty() {
        let _ = ctx.write_stdout("No projects registered.");
        return Ok(());
    }

    let _ = ctx.write_stdout("Registered Projects:");
    for p in projects {
        let last_accessed = DateTime::<Utc>::from_timestamp(p.last_accessed as i64, 0)
            .unwrap_or_default()
            .format("%Y-%m-%d %H:%M");
        let _ = ctx.write_stdout(&format!(
            "  {:<20} {} ({})",
            p.name,
            p.path.display(),
            last_accessed
        ));
    }
    Ok(())
}

fn remove(ctx: &Context, args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err(anyhow::anyhow!("Usage: pm remove <name>"));
    }
    let name = &args[0];

    let mut projects = load_projects()?;
    let len_before = projects.len();
    projects.retain(|p| &p.name != name);

    if projects.len() == len_before {
        return Err(anyhow::anyhow!("Project '{}' not found", name));
    }

    save_projects(&projects)?;
    let _ = ctx.write_stdout(&format!("Project '{}' removed", name));
    Ok(())
}

fn work(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    if args.is_empty() {
        return Err(anyhow::anyhow!("Usage: pm work <name>"));
    }
    let name = &args[0];

    let mut projects = load_projects()?;
    let project_idx = projects
        .iter()
        .position(|p| &p.name == name)
        .context(format!("Project '{}' not found", name))?;

    // Update timestamp
    projects[project_idx].update_timestamp();
    let project = projects[project_idx].clone();
    save_projects(&projects)?;

    // Change directory
    // Change directory
    proxy.changepwd(&project.path.to_string_lossy())?;
    // Hook triggering is now handled automatically by the shell's chpwd hook mechanism
    // when detecting a project context switch.

    let _ = ctx.write_stdout(&format!("Switched to project '{}'", project.name));

    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EnvrcActivation {
    vars: Vec<(String, String)>,
    path_adds: Vec<String>,
}

fn parse_dotenv_file(path: &Path) -> Result<Vec<(String, String)>> {
    let contents = fs::read_to_string(path)?;
    Ok(contents.lines().filter_map(parse_assignment_line).collect())
}

fn parse_envrc_file(path: &Path) -> Result<EnvrcActivation> {
    let contents = fs::read_to_string(path)?;
    let mut activation = EnvrcActivation::default();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((command, rest)) = split_command_line(trimmed) else {
            continue;
        };

        match command.to_ascii_lowercase().as_str() {
            "export" => {
                if let Some(var) = parse_assignment(rest) {
                    activation.vars.push(var);
                }
            }
            "path_add" => {
                let path = unquote(rest.trim());
                if !path.is_empty() {
                    activation.path_adds.push(path);
                }
            }
            _ => {}
        }
    }

    Ok(activation)
}

fn parse_assignment_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let assignment = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    parse_assignment(assignment)
}

fn parse_assignment(assignment: &str) -> Option<(String, String)> {
    let (key, value) = assignment.split_once('=')?;
    let key = key.trim();
    if !is_valid_env_key(key) {
        return None;
    }
    Some((key.to_string(), unquote(value.trim())))
}

fn split_command_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let command = parts.next()?.trim();
    let rest = parts.next().unwrap_or("").trim();
    Some((command, rest))
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn find_project_venv(root: &Path) -> Option<PathBuf> {
    [".venv", "venv"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|path| path.is_dir())
}

fn normalize_activation_path(root: &Path, path: &str) -> PathBuf {
    let expanded = shellexpand::tilde(path).into_owned();
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn display_activation_path(root: &Path, path: &str) -> String {
    normalize_activation_path(root, path).display().to_string()
}

fn prepend_path(proxy: &mut dyn ShellProxy, root: &Path, path: &str) -> bool {
    let path = normalize_activation_path(root, path);
    let path = path.to_string_lossy().into_owned();
    let current_path = proxy
        .get_var("PATH")
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();

    if current_path.split(':').any(|entry| entry == path) {
        return false;
    }

    let updated = if current_path.is_empty() {
        path
    } else {
        format!("{path}:{current_path}")
    };
    proxy.set_env_var("PATH".to_string(), updated);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationProvider {
    Auto,
    Native,
    Mise,
}

fn activate(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    let mut provider = ActivationProvider::Auto;
    let mut dry_run = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry_run = true,
            "--provider" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(anyhow::anyhow!("--provider requires auto, native, or mise"));
                };
                provider = parse_activation_provider(value)?;
            }
            value if value.starts_with("--provider=") => {
                provider = parse_activation_provider(value.trim_start_matches("--provider="))?;
            }
            value => return Err(anyhow::anyhow!("unknown activate option: {value}")),
        }
        index += 1;
    }

    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;

    if matches!(
        provider,
        ActivationProvider::Auto | ActivationProvider::Native
    ) {
        activate_native(ctx, proxy, dry_run)?;
    }
    if matches!(
        provider,
        ActivationProvider::Auto | ActivationProvider::Mise
    ) {
        let status = MiseStatus::detect(&root);
        if matches!(status.trust.as_str(), "trusted" | "safe") {
            activate_mise(ctx, proxy, &root, &status, dry_run)?;
        } else if provider == ActivationProvider::Mise {
            return Err(anyhow::anyhow!(
                "mise provider is {}. dsh will not trust or install it automatically",
                status.trust
            ));
        } else if status.trust != "not-configured" {
            let _ = ctx.write_stdout(&format!(
                "mise overlay skipped trust={} (dsh never runs `mise trust` automatically)",
                status.trust
            ));
        }
    }
    Ok(())
}

fn parse_activation_provider(value: &str) -> Result<ActivationProvider> {
    match value {
        "auto" => Ok(ActivationProvider::Auto),
        "native" => Ok(ActivationProvider::Native),
        "mise" => Ok(ActivationProvider::Mise),
        _ => Err(anyhow::anyhow!(
            "unknown provider `{value}`; expected auto, native, or mise"
        )),
    }
}

fn activate_mise(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    root: &Path,
    status: &MiseStatus,
    dry_run: bool,
) -> Result<()> {
    let mise = status
        .executable
        .as_deref()
        .context("mise executable unavailable")?;
    let output = mise_output(mise, root, &["--no-hooks", "env", "--json"])?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "mise env failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: JsonValue = serde_json::from_slice(&output.stdout)?;
    let object = value
        .as_object()
        .context("mise env --json returned a non-object value")?;
    let mut changed = 0;
    for (key, value) in object {
        let Some(value) = value.as_str() else {
            continue;
        };
        if proxy.get_var(key).as_deref() == Some(value) {
            continue;
        }
        changed += 1;
        if dry_run {
            let _ = ctx.write_stdout(&format!(
                "mise set {}={}",
                key,
                safety_policy::mask_env_value(key, value)
            ));
        } else {
            proxy.set_env_var(key.clone(), value.to_string());
        }
    }
    let mode = if dry_run { "dry-run" } else { "applied" };
    let _ = ctx.write_stdout(&format!(
        "mise overlay {mode} vars={changed} hooks=disabled trust={}",
        status.trust
    ));
    Ok(())
}

fn activate_native(ctx: &Context, proxy: &mut dyn ShellProxy, dry_run: bool) -> Result<()> {
    if dry_run {
        return activate_dry_run(ctx, proxy);
    }

    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;
    let mut applied = Vec::new();

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        for (key, value) in &vars {
            if env_assignment_requires_confirmation(key, value)
                && !proxy.confirm_action(&format!(
                    "Apply sensitive or high-risk environment variable `{key}` from .env? \r\nProceed?"
                ))?
            {
                applied.push(format!(".env skipped {key}"));
                continue;
            }
            proxy.set_env_var(key.clone(), value.clone());
        }
        if !vars.is_empty() {
            applied.push(format!(".env vars={}", vars.len()));
        }
    }

    let envrc = root.join(".envrc");
    if envrc.exists() {
        if proxy.is_direnv_allowed(&root) {
            let plan = parse_envrc_file(&envrc)?;
            for (key, value) in &plan.vars {
                if env_assignment_requires_confirmation(key, value)
                    && !proxy.confirm_action(&format!(
                        "Apply sensitive or high-risk environment variable `{key}` from .envrc? \r\nProceed?"
                    ))?
                {
                    applied.push(format!(".envrc skipped {key}"));
                    continue;
                }
                proxy.set_env_var(key.clone(), value.clone());
            }
            for path in &plan.path_adds {
                if activation_path_outside_root(&root, path)
                    && !proxy.confirm_action(&format!(
                        "Add PATH entry outside project root `{}` from .envrc? \r\nProceed?",
                        display_activation_path(&root, path)
                    ))?
                {
                    applied.push(format!(
                        "path_add skipped {}",
                        display_activation_path(&root, path)
                    ));
                    continue;
                }
                if prepend_path(proxy, &root, path) {
                    applied.push(format!("path_add {}", display_activation_path(&root, path)));
                }
            }
            if !plan.vars.is_empty() {
                applied.push(format!(".envrc vars={}", plan.vars.len()));
            }
        } else {
            let _ = ctx.write_stdout(&format!(
                "Skipped .envrc at {} (not allow-direnv root).",
                envrc.display()
            ));
        }
    }

    if let Some(venv) = find_project_venv(&root) {
        proxy.set_env_var(
            "VIRTUAL_ENV".to_string(),
            venv.to_string_lossy().into_owned(),
        );
        let bin = venv.join("bin");
        if bin.is_dir() && prepend_path(proxy, &root, bin.to_string_lossy().as_ref()) {
            applied.push(format!("venv {}", venv.display()));
        } else {
            applied.push(format!("VIRTUAL_ENV {}", venv.display()));
        }
    }

    if applied.is_empty() {
        let _ = ctx.write_stdout(&format!("No activation files found in {}.", root.display()));
    } else {
        let _ = ctx.write_stdout(&format!(
            "Activated project environment for {}: {}",
            root.display(),
            applied.join(", ")
        ));
    }

    Ok(())
}

fn activate_dry_run(ctx: &Context, proxy: &mut dyn ShellProxy) -> Result<()> {
    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;

    let _ = ctx.write_stdout(&format!("activation dry-run root {}", root.display()));

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        if vars.is_empty() {
            let _ = ctx.write_stdout(".env vars=0");
        } else {
            for (key, value) in vars {
                let marker = if env_assignment_requires_confirmation(&key, &value) {
                    " confirm"
                } else {
                    ""
                };
                let _ = ctx.write_stdout(&format!(
                    ".env set {}={}{}",
                    key,
                    safety_policy::mask_env_value(&key, &value),
                    marker
                ));
            }
        }
    } else {
        let _ = ctx.write_stdout(".env missing");
    }

    let envrc = root.join(".envrc");
    if envrc.exists() {
        if proxy.is_direnv_allowed(&root) {
            let plan = parse_envrc_file(&envrc)?;
            for (key, value) in plan.vars {
                let marker = if env_assignment_requires_confirmation(&key, &value) {
                    " confirm"
                } else {
                    ""
                };
                let _ = ctx.write_stdout(&format!(
                    ".envrc set {}={}{}",
                    key,
                    safety_policy::mask_env_value(&key, &value),
                    marker
                ));
            }
            for path in plan.path_adds {
                let marker = if activation_path_outside_root(&root, &path) {
                    " confirm-outside-root"
                } else {
                    ""
                };
                let _ = ctx.write_stdout(&format!(
                    ".envrc path_add {}{}",
                    display_activation_path(&root, &path),
                    marker
                ));
            }
        } else {
            let _ = ctx.write_stdout(&format!(".envrc skipped {} not-allowed", envrc.display()));
        }
    } else {
        let _ = ctx.write_stdout(".envrc missing");
    }

    if let Some(venv) = find_project_venv(&root) {
        let _ = ctx.write_stdout(&format!("venv {}", venv.display()));
        let bin = venv.join("bin");
        if bin.is_dir() {
            let _ = ctx.write_stdout(&format!("venv path_add {}", bin.display()));
        }
    } else {
        let _ = ctx.write_stdout("venv missing");
    }

    if let Ok(summary) = activation_safety_summary(&root, proxy) {
        let _ = ctx.write_stdout(&summary);
    }

    Ok(())
}

fn activation_safety_summary(root: &Path, proxy: &dyn ShellProxy) -> Result<String> {
    let mut env_vars = 0usize;
    let mut confirm_vars = 0usize;
    let mut outside_paths = 0usize;

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        env_vars += vars.len();
        confirm_vars += vars
            .iter()
            .filter(|(key, value)| env_assignment_requires_confirmation(key, value))
            .count();
    }

    let envrc = root.join(".envrc");
    let envrc_state = if envrc.exists() {
        if proxy.is_direnv_allowed(root) {
            let plan = parse_envrc_file(&envrc)?;
            env_vars += plan.vars.len();
            confirm_vars += plan
                .vars
                .iter()
                .filter(|(key, value)| env_assignment_requires_confirmation(key, value))
                .count();
            outside_paths += plan
                .path_adds
                .iter()
                .filter(|path| activation_path_outside_root(root, path))
                .count();
            "allowed"
        } else {
            "not-allowed"
        }
    } else {
        "missing"
    };

    Ok(format!(
        "activation safety env_vars={env_vars} confirm_vars={confirm_vars} envrc={envrc_state} outside_path_adds={outside_paths}"
    ))
}

fn env_assignment_requires_confirmation(key: &str, value: &str) -> bool {
    is_high_risk_env_key(key)
        || safety_policy::is_sensitive_key(key)
        || safety_policy::contains_sensitive_text(value)
}

fn is_high_risk_env_key(key: &str) -> bool {
    matches!(
        key,
        "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "DYLD_INSERT_LIBRARIES"
            | "PYTHONPATH"
            | "PERL5LIB"
            | "RUBYLIB"
            | "NODE_OPTIONS"
    )
}

fn activation_path_outside_root(root: &Path, path: &str) -> bool {
    let root = lexical_normalize(root);
    let normalized = lexical_normalize(&normalize_activation_path(&root, path));
    !normalized.starts_with(&root)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn jump(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    // If exact name provided, delegate to work
    if !args.is_empty() {
        return work(ctx, args, proxy);
    }

    let mut projects = load_projects()?;
    projects.sort_by_key(|project| std::cmp::Reverse(project.last_accessed));

    if projects.is_empty() {
        let _ = ctx.write_stdout("No projects registered.");
        return Ok(());
    }

    let items: Vec<String> = projects.iter().map(|p| p.name.clone()).collect();

    if let Some(selected) = proxy.select_item(items)? {
        let _ = ctx.write_stdout(&format!("Selected: {}", selected));
        work(ctx, &[selected], proxy)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::mcp::McpServerConfig;
    use dsh_types::observed_output::ObservedOutput;
    use std::io::Write;
    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::PermissionsExt;

    struct TestProxy {
        cwd: PathBuf,
        direnv_allowed: bool,
        set_env_calls: usize,
        insert_path_calls: usize,
    }

    impl ShellProxy for TestProxy {
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

        fn dispatch(
            &mut self,
            _ctx: &Context,
            _cmd: &str,
            _argv: Vec<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn save_path_history(&mut self, _path: &str) {}

        fn changepwd(&mut self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn insert_path(&mut self, _index: usize, _path: &str) {
            self.insert_path_calls += 1;
        }

        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }

        fn set_var(&mut self, _key: String, _value: String) {}

        fn set_env_var(&mut self, _key: String, _value: String) {
            self.set_env_calls += 1;
        }

        fn is_direnv_allowed(&self, _path: &Path) -> bool {
            self.direnv_allowed
        }

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

        fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
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
            Ok(self.cwd.clone())
        }

        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
    }

    fn observed_context() -> (Context, dsh_types::observed_output::SharedOutputObserver) {
        let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let observer = ObservedOutput::shared(8192);
        ctx.output_observer = Some(observer.clone());
        ctx.outfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        ctx.errfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        (ctx, observer)
    }

    fn observed_stdout(observer: &dsh_types::observed_output::SharedOutputObserver) -> String {
        observer.lock().unwrap().snapshot().stdout
    }

    #[test]
    fn dotenv_parser_accepts_export_and_quotes() {
        assert_eq!(
            parse_assignment_line("export FOO=\"bar baz\""),
            Some(("FOO".to_string(), "bar baz".to_string()))
        );
        assert_eq!(
            parse_assignment_line("BAD-NAME=value"),
            None,
            "invalid shell env names should be skipped"
        );
    }

    #[test]
    fn envrc_parser_only_collects_safe_forms() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "export FOO=bar").unwrap();
        writeln!(file, "path_add ./bin").unwrap();
        writeln!(file, "source ./danger.sh").unwrap();

        let activation = parse_envrc_file(file.path()).unwrap();
        assert_eq!(
            activation,
            EnvrcActivation {
                vars: vec![("FOO".to_string(), "bar".to_string())],
                path_adds: vec!["./bin".to_string()],
            }
        );
    }

    #[test]
    fn project_name_from_path_falls_back_to_project() {
        assert_eq!(project_name_from_path(Path::new("/tmp/demo")), "demo");
        assert_eq!(project_name_from_path(Path::new("/")), "project");
    }

    #[test]
    fn activation_safety_detects_sensitive_env_and_outside_path() {
        assert!(env_assignment_requires_confirmation("API_KEY", "abc123"));
        assert!(env_assignment_requires_confirmation(
            "LD_PRELOAD",
            "/tmp/hook.so"
        ));
        assert!(!env_assignment_requires_confirmation("APP_MODE", "dev"));
        assert!(activation_path_outside_root(
            Path::new("/tmp/project"),
            "../bin"
        ));
        assert!(!activation_path_outside_root(
            Path::new("/tmp/project"),
            "./bin"
        ));
    }

    #[test]
    fn task_source_counts_groups_by_source() {
        let tasks = vec![
            task::TaskInfo::new("cargo", "test", "cargo test", "/tmp"),
            task::TaskInfo::new("cargo", "check", "cargo check", "/tmp"),
            task::TaskInfo::new("npm", "build", "npm run build", "/tmp"),
        ];

        let counts = task_source_counts(&tasks);
        assert_eq!(counts.get("cargo"), Some(&2));
        assert_eq!(counts.get("npm"), Some(&1));
    }

    #[test]
    fn help_text_mentions_onboarding_commands() {
        let help = help_text();
        assert!(help.contains("pm init"));
        assert!(help.contains("status"));
        assert!(help.contains("activate"));
        assert!(help.contains("--dry-run"));
    }

    #[test]
    fn activate_dry_run_masks_values_and_does_not_mutate_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
        std::fs::write(dir.path().join(".env"), "API_KEY=secret\nAPP_MODE=dev\n").unwrap();
        std::fs::write(
            dir.path().join(".envrc"),
            "export SERVICE_TOKEN=token\npath_add ../bin\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(".venv/bin")).unwrap();

        let mut proxy = TestProxy {
            cwd: dir.path().to_path_buf(),
            direnv_allowed: true,
            set_env_calls: 0,
            insert_path_calls: 0,
        };
        let (ctx, observer) = observed_context();

        let status = command(
            &ctx,
            vec![
                "pm".to_string(),
                "activate".to_string(),
                "--dry-run".to_string(),
            ],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.set_env_calls, 0);
        assert_eq!(proxy.insert_path_calls, 0);

        let output = observed_stdout(&observer);
        assert!(output.contains(".env set API_KEY=*** confirm"));
        assert!(output.contains(".env set APP_MODE=dev"));
        assert!(output.contains(".envrc set SERVICE_TOKEN=*** confirm"));
        assert!(output.contains("confirm-outside-root"));
        assert!(output.contains("venv path_add"));
        assert!(output.contains("activation safety env_vars=3 confirm_vars=2"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("SERVICE_TOKEN=token"));
    }

    #[test]
    fn mise_activation_checks_trust_disables_hooks_and_dry_run_does_not_mutate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mise.toml"), "[tools]\nnode = '22'\n").unwrap();
        let executable = dir.path().join("mise-fake");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$PWD/mise-args.log"
if [ "$1" = "trust" ]; then printf '%s\n' "$PWD: trusted"; exit 0; fi
if [ "$1" = "--no-hooks" ] && [ "$2" = "ls" ]; then printf '%s\n' '[{"name":"python"}]'; exit 0; fi
if [ "$1" = "--no-hooks" ] && [ "$2" = "env" ]; then
  printf '%s\n' '{"PATH":"/tmp/mise/bin","API_KEY":"secret"}'
  exit 0
fi
exit 1
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let status = MiseStatus::detect_with_executable(dir.path(), Some(executable));
        assert_eq!(status.trust, "trusted");
        assert_eq!(status.missing_tools, vec!["python"]);

        let mut proxy = TestProxy {
            cwd: dir.path().to_path_buf(),
            direnv_allowed: false,
            set_env_calls: 0,
            insert_path_calls: 0,
        };
        let (ctx, observer) = observed_context();
        activate_mise(&ctx, &mut proxy, dir.path(), &status, true).unwrap();
        assert_eq!(proxy.set_env_calls, 0);
        let output = observed_stdout(&observer);
        assert!(output.contains("API_KEY=***"));
        assert!(!output.contains("API_KEY=secret"));
        let args = std::fs::read_to_string(dir.path().join("mise-args.log")).unwrap();
        assert!(args.contains("trust --show"));
        assert!(args.contains("--no-hooks ls --missing --json"));
        assert!(args.contains("--no-hooks env --json"));
        assert!(
            !args
                .lines()
                .any(|line| line.starts_with("trust") && line != "trust --show")
        );
    }

    #[test]
    fn safe_mise_config_requires_only_plain_tools_and_non_templated_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mise.toml");
        std::fs::write(
            &config,
            "min_version = '2026.1.0'\n[tools]\nnode = ['22', '24']\n[tasks.test]\nrun = 'cargo test'\n",
        )
        .unwrap();
        assert!(mise_config_is_safe(dir.path()));

        std::fs::write(&config, "[env]\nTOKEN = 'secret'\n").unwrap();
        assert!(!mise_config_is_safe(dir.path()));

        std::fs::write(&config, "[tasks.test]\nrun = 'echo {{env.HOME}}'\n").unwrap();
        assert!(!mise_config_is_safe(dir.path()));

        std::fs::write(&config, "[tasks.test.tools]\nnode = '22'\n").unwrap();
        assert!(!mise_config_is_safe(dir.path()));
    }

    #[test]
    fn missing_mise_tools_accepts_current_object_json_shape() {
        let mut tools = Vec::new();
        collect_missing_tool_names(
            &serde_json::json!({
                "node": [{"version": "22", "installed": false}],
                "python": []
            }),
            &mut tools,
        );
        assert_eq!(tools, vec!["node"]);
    }

    #[test]
    fn project_status_json_reports_provider_lock_and_dev_container_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
        std::fs::create_dir_all(dir.path().join(".devcontainer")).unwrap();
        std::fs::write(dir.path().join(".devcontainer/devcontainer.json"), "{}").unwrap();
        let context = project_context::resolve_project_context(dir.path());
        let status = build_project_status(&context, &[]);
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["provider"], "native");
        assert_eq!(value["trust"], "not-configured");
        assert!(value["missing_tools"].is_array());
        assert!(
            value["dev_container"]
                .as_str()
                .unwrap()
                .ends_with(".devcontainer/devcontainer.json")
        );
    }
}
