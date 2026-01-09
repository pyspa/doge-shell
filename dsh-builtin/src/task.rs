use super::ShellProxy;
use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use regex::Regex;
use skim::prelude::*;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tabled::Tabled;

pub fn description() -> &'static str {
    "Run project-specific tasks (npm, cargo, make, deno, just, etc.)"
}

#[derive(Debug, Clone, Tabled)]
struct Task {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Task")]
    name: String,
    #[tabled(rename = "Command")]
    command: String,
}

#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub source: String,
    pub name: String,
    pub command: String,
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
    // If specific task is requested (e.g. task build)
    // We need to find the task and run it.
    // If no args, show selector.
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

    if argv.len() > 1 {
        // Direct execution: task <name>
        let target_name = &argv[1];
        // Find task by name. If checks for multiple sources, priorities?
        // or interactive disambiguation?
        // Simple strategy: exact match on name.
        let matched: Vec<&Task> = tasks.iter().filter(|t| &t.name == target_name).collect();

        if matched.is_empty() {
            let _ = ctx.write_stderr(&format!("Task '{}' not found.\n", target_name));
            return ExitStatus::ExitedWith(1);
        } else if matched.len() == 1 {
            return execute_task(ctx, matched[0], proxy);
        } else {
            // Multiple matches (e.g. build in npm and cargo)
            let _ = ctx.write_stdout(&format!(
                "Multiple tasks found for '{}'. Please select one:\n",
                target_name
            ));
            // Fallthrough to selection but filtered?
            // For now, let's just pick the first one or error.
            // Let's execute the first one but warn.
            let _ = ctx.write_stderr(&format!(
                "Ambiguous task name, running from source: {}\n",
                matched[0].source
            ));
            return execute_task(ctx, matched[0], proxy);
        }
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
    for task in tasks {
        let _ = tx.send(Arc::new(task));
    }
    drop(tx);

    let selected = Skim::run_with(&options, Some(rx))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    if let Some(item) = selected.first() {
        // Downcast back to Task - but SkimItem logic handles output()
        let command = item.output().to_string();
        // Print what we run
        let _ = ctx.write_stdout(&format!("Running: {}\n", command));

        match proxy.dispatch(ctx, "sh", vec!["sh".to_string(), "-c".to_string(), command]) {
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

fn execute_task(ctx: &Context, task: &Task, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let _ = ctx.write_stdout(&format!(
        "Running [{}] {} -> {}\n",
        task.source, task.name, task.command
    ));
    match proxy.dispatch(
        ctx,
        "sh",
        vec!["sh".to_string(), "-c".to_string(), task.command.clone()],
    ) {
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
    detect_tasks_in_dir(current_dir)
}

fn detect_tasks_in_dir(current_dir: &Path) -> Result<Vec<TaskInfo>> {
    let mut tasks = Vec::new();

    // 1. package.json (npm, yarn, pnpm, bun)
    if let Ok(content) = fs::read_to_string(current_dir.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(scripts) = json.get("scripts").and_then(|s| s.as_object())
    {
        let manager = detect_js_manager(current_dir);
        for (name, cmd) in scripts {
            let _ = cmd; // unused
            tasks.push(TaskInfo {
                source: manager.clone(),
                name: name.clone(),
                // e.g. "npm run build"
                command: format!("{} run {}", manager, name),
            });
        }
    }

    // 2. Cargo.toml
    if current_dir.join("Cargo.toml").exists() {
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
    if current_dir.join("Makefile").exists() || current_dir.join("makefile").exists() {
        // Use make -pRrq : to list targets
        // Execution runs in current dir, or use -C? Command current_dir should work.
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
    if justfile_exists {
        // Try `just --summary`
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

    Ok(tasks)
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

fn remove_jsonc_comments(json: &str) -> String {
    let re = Regex::new(r"(?s)//[^\n]*|/\*.*?\*/").unwrap();
    re.replace_all(json, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

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

        let tasks = detect_tasks_in_dir(dir.path()).unwrap();
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

        let tasks = detect_tasks_in_dir(dir.path()).unwrap();
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

        let tasks = detect_tasks_in_dir(dir.path()).unwrap();
        let task = tasks.first().unwrap();
        assert_eq!(task.source, "yarn");
        assert_eq!(task.command, "yarn run build");
    }
}
