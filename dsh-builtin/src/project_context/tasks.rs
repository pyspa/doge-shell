//! Finding the task names a project defines, per task runner: mise, Taskfile,
//! turbo, and nx (both the workspace file and the descendant `project.json`
//! files), deduplicated so the same name from two runners is listed once.
use super::*;

pub fn detect_task_names_in_dir(current_dir: &Path) -> Result<Vec<TaskDefinition>> {
    let project = resolve_project_context(current_dir);
    let root = &project.project_root;
    let mut tasks = Vec::new();

    tasks.extend(detect_mise_tasks(root)?);
    tasks.extend(detect_taskfile_tasks(root)?);
    tasks.extend(detect_turbo_tasks(root)?);
    tasks.extend(detect_nx_tasks(root)?);

    Ok(dedup_tasks(tasks))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDefinition {
    pub source: String,
    pub name: String,
    pub command: String,
}

fn detect_mise_tasks(root: &Path) -> Result<Vec<TaskDefinition>> {
    let Some(path) = [root.join("mise.toml"), root.join(".mise.toml")]
        .into_iter()
        .find(|path| path.exists())
    else {
        return Ok(Vec::new());
    };

    let content = fs::read_to_string(path)?;
    let value = toml::from_str::<TomlValue>(&content)?;
    let mut tasks = Vec::new();

    if let Some(table) = value.get("tasks").and_then(TomlValue::as_table) {
        for (name, value) in table {
            if value.is_str() || value.is_table() || value.is_array() {
                tasks.push(TaskDefinition {
                    source: "mise".to_string(),
                    name: name.clone(),
                    command: format!("mise run {}", name),
                });
            }
        }
    }

    Ok(tasks)
}

fn detect_taskfile_tasks(root: &Path) -> Result<Vec<TaskDefinition>> {
    let path = ["Taskfile.yml", "Taskfile.yaml"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|path| path.exists());
    let Some(path) = path else {
        return Ok(Vec::new());
    };

    let content = fs::read_to_string(path)?;
    let mut in_tasks = false;
    let mut tasks = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !in_tasks {
            if trimmed == "tasks:" {
                in_tasks = true;
            }
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        if let Some((raw_name, _)) = trimmed.split_once(':') {
            if !line.starts_with("  ") && !line.starts_with('\t') {
                continue;
            }
            let name = raw_name.trim();
            if name.is_empty()
                || name.starts_with('{')
                || name == "desc"
                || name == "cmd"
                || name == "cmds"
                || name == "deps"
                || name == "vars"
                || name == "env"
            {
                continue;
            }
            tasks.push(TaskDefinition {
                source: "taskfile".to_string(),
                name: name.to_string(),
                command: format!("task {}", name),
            });
        }
    }

    Ok(tasks)
}

fn detect_turbo_tasks(root: &Path) -> Result<Vec<TaskDefinition>> {
    let path = root.join("turbo.json");
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)?;
    let value = serde_json::from_str::<JsonValue>(&content)?;
    let mut tasks = Vec::new();
    if let Some(table) = value.get("tasks").and_then(JsonValue::as_object) {
        for name in table.keys() {
            tasks.push(TaskDefinition {
                source: "turbo".to_string(),
                name: name.clone(),
                command: format!("turbo run {}", name),
            });
        }
    }
    Ok(tasks)
}

pub(super) fn detect_nx_tasks(root: &Path) -> Result<Vec<TaskDefinition>> {
    let mut tasks = Vec::new();
    for path in [root.join("workspace.json"), root.join("angular.json")] {
        tasks.extend(detect_nx_workspace_tasks(&path)?);
    }

    if root.join("project.json").exists() {
        tasks.extend(detect_nx_project_tasks(&root.join("project.json"), root)?);
    }
    for path in find_descendant_project_json_files(root, 4) {
        if path != root.join("project.json") {
            let fallback_root = path.parent().unwrap_or(root);
            tasks.extend(detect_nx_project_tasks(&path, fallback_root)?);
        }
    }
    Ok(tasks)
}

fn detect_nx_workspace_tasks(path: &Path) -> Result<Vec<TaskDefinition>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    let value = serde_json::from_str::<JsonValue>(&content)?;
    let mut tasks = Vec::new();
    if let Some(projects) = value.get("projects").and_then(JsonValue::as_object) {
        for (project_name, project_value) in projects {
            if let Some(project_object) = project_value.as_object() {
                tasks.extend(nx_tasks_from_project_value(
                    project_name,
                    &JsonValue::Object(project_object.clone()),
                ));
            }
        }
    }
    Ok(tasks)
}

fn detect_nx_project_tasks(path: &Path, fallback_root: &Path) -> Result<Vec<TaskDefinition>> {
    let content = fs::read_to_string(path)?;
    let value = serde_json::from_str::<JsonValue>(&content)?;
    let project_name = value
        .get("name")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .or_else(|| {
            fallback_root
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        });
    let Some(project_name) = project_name else {
        return Ok(Vec::new());
    };
    Ok(nx_tasks_from_project_value(&project_name, &value))
}

fn nx_tasks_from_project_value(project_name: &str, value: &JsonValue) -> Vec<TaskDefinition> {
    let targets = value
        .get("targets")
        .or_else(|| value.get("architect"))
        .and_then(JsonValue::as_object);
    let Some(targets) = targets else {
        return Vec::new();
    };
    targets
        .keys()
        .map(|name| TaskDefinition {
            source: "nx".to_string(),
            name: format!("{project_name}:{name}"),
            command: format!("nx run {}:{}", project_name, name),
        })
        .collect()
}

fn find_descendant_project_json_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut values = Vec::new();
    collect_descendant_project_json_files(root, 0, max_depth, &mut values);
    values
}

fn collect_descendant_project_json_files(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    values: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || matches!(name, "node_modules" | "target" | "dist" | "build") {
            continue;
        }
        if path.is_file() && name == "project.json" {
            values.push(path);
        } else if path.is_dir() {
            collect_descendant_project_json_files(&path, depth + 1, max_depth, values);
        }
    }
}

fn dedup_tasks(tasks: Vec<TaskDefinition>) -> Vec<TaskDefinition> {
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
