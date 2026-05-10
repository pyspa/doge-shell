use super::ShellProxy;
use crate::project_context;
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use dsh_types::{Context, ExitStatus, Project};
use std::fs;
use std::path::{Path, PathBuf};

const PROJECTS_FILE: &str = "projects.json";

pub fn description() -> &'static str {
    "Manage projects (add, list, remove, work, activate)"
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
        let _ = ctx.write_stderr("Usage: pm <add|list|remove|work|jump|activate> [args]");
        return ExitStatus::ExitedWith(1);
    }

    match argv[1].as_str() {
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
}
