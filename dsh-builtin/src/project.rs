use super::ShellProxy;
use crate::project_context;
use crate::task;
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use dsh_types::{Context, ExitStatus, Project};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
        "status" | "st" => match status(ctx, proxy) {
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
        "  status               Show current project root, registration, activation, runtimes, and tasks\n",
        "  add [path] [name]    Register a project path\n",
        "  list | ls            List registered projects\n",
        "  remove | rm <name>   Remove a project\n",
        "  work <name>          Switch to a project\n",
        "  jump                 Select and switch to a project interactively\n",
        "  activate             Apply safe .env, allowed .envrc, and venv activation\n",
        "\n",
        "Aliases:\n",
        "  pj [name]            Alias for pm jump\n",
        "\n",
        "Examples:\n",
        "  pm init\n",
        "  pm status\n",
        "  pm activate\n",
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

fn status(ctx: &Context, proxy: &mut dyn ShellProxy) -> Result<()> {
    let current_dir = proxy.get_current_dir()?;
    let context = project_context::resolve_project_context(&current_dir);
    let projects = load_projects()?;
    print_project_status(ctx, proxy, &context, &projects);
    Ok(())
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

fn activate(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    if !args.is_empty() {
        return Err(anyhow::anyhow!("Usage: pm activate"));
    }

    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;
    let mut applied = Vec::new();

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        for (key, value) in &vars {
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
                proxy.set_env_var(key.clone(), value.clone());
            }
            for path in &plan.path_adds {
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
    use std::io::Write;

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
    fn task_source_counts_groups_by_source() {
        let tasks = vec![
            task::TaskInfo {
                source: "cargo".to_string(),
                name: "test".to_string(),
                command: "cargo test".to_string(),
            },
            task::TaskInfo {
                source: "cargo".to_string(),
                name: "check".to_string(),
                command: "cargo check".to_string(),
            },
            task::TaskInfo {
                source: "npm".to_string(),
                name: "build".to_string(),
                command: "npm run build".to_string(),
            },
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
    }
}
