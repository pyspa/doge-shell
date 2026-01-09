use super::ShellProxy;
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use dsh_types::{Context, ExitStatus, Project};
use std::fs;
use std::path::{Path, PathBuf};

const PROJECTS_FILE: &str = "projects.json";

pub fn description() -> &'static str {
    "Manage projects (add, list, remove, work)"
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
        let _ = ctx.write_stderr("Usage: pm <add|list|remove|work|jump> [args]");
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
    projects.sort_by(|a, b| b.last_accessed.cmp(&a.last_accessed));

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

fn jump(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    // If exact name provided, delegate to work
    if !args.is_empty() {
        return work(ctx, args, proxy);
    }

    let mut projects = load_projects()?;
    projects.sort_by(|a, b| b.last_accessed.cmp(&a.last_accessed));

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
