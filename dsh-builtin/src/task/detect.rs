//! The main per-ecosystem task scan (`detect_tasks_in_dir`): package.json (npm/pnpm/yarn/bun), Cargo.toml, Gradle, Makefile, deno.json(c), and Justfile, plus the small parsing/filtering helpers
//! it alone uses.
use super::*;

pub(super) fn detect_tasks_in_dir(
    current_dir: &Path,
    mode: TaskDetectionMode,
    source_filter: Option<&[&str]>,
) -> Result<TaskDiscoverySummary> {
    let project = project_context::resolve_project_context(current_dir);
    let current_dir = project.project_root.as_path();
    let cwd = current_dir.display().to_string();
    let project_tasks = if any_source_enabled(source_filter, &["mise", "taskfile", "turbo", "nx"]) {
        discover_provider_tasks(current_dir)?
            .into_iter()
            .filter(|task| source_enabled(source_filter, &task.source))
            .map(|task| TaskInfo::new(task.source, task.name, task.command, cwd.clone()))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
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
                let mut task = TaskInfo::new(
                    manager.clone(),
                    name.clone(),
                    format!("{} run {}", manager, name),
                    cwd.clone(),
                );
                task.description = scripts
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                tasks.push(task);
            }
        }
    }

    // 2. Cargo.toml
    if source_enabled(source_filter, "cargo") && current_dir.join("Cargo.toml").exists() {
        // Standard cargo commands
        for cmd in ["build", "run", "test", "check", "clippy", "fmt", "doc"] {
            tasks.push(TaskInfo::new(
                "cargo",
                cmd,
                format!("cargo {cmd}"),
                cwd.clone(),
            ));
        }
    }

    // 3. Gradle
    if source_enabled(source_filter, "gradle") && has_gradle_project(current_dir) {
        match mode {
            TaskDetectionMode::Full => {
                let command_name = if current_dir.join("gradlew").is_file() {
                    "./gradlew"
                } else {
                    "gradle"
                };
                if let Ok(output) = command_output_with_timeout(
                    Path::new(command_name),
                    &["-q", "tasks", "--all"],
                    current_dir,
                    Duration::from_millis(1500),
                ) {
                    let content = String::from_utf8_lossy(&output.stdout);
                    for name in parse_gradle_task_names(&content) {
                        tasks.push(TaskInfo::new(
                            "gradle",
                            name.clone(),
                            format!("{command_name} {name}"),
                            cwd.clone(),
                        ));
                    }
                }
            }
            TaskDetectionMode::MetadataOnly => deferred_sources.push("gradle".to_string()),
        }
    }

    // 4. Makefile
    if source_enabled(source_filter, "make")
        && (current_dir.join("Makefile").exists() || current_dir.join("makefile").exists())
    {
        match mode {
            TaskDetectionMode::Full => {
                // Use make -pRrq : to list targets. This can evaluate Makefile constructs,
                // so passive diagnostics must use MetadataOnly mode instead.
                if let Ok(output) = command_output_with_timeout(
                    Path::new("make"),
                    &["-pRrq", ":"],
                    current_dir,
                    Duration::from_millis(1500),
                ) {
                    let content = String::from_utf8_lossy(&output.stdout);
                    for line in content.lines() {
                        if let Some(target) = line.strip_suffix(':')
                            && !target.starts_with(['.', '#', '%'])
                            && !target.contains('%')
                            && !target.contains(' ')
                        {
                            tasks.push(TaskInfo::new(
                                "make",
                                target,
                                format!("make {target}"),
                                cwd.clone(),
                            ));
                        }
                    }
                }
            }
            TaskDetectionMode::MetadataOnly => deferred_sources.push("make".to_string()),
        }
    }

    // 5. deno.json / deno.jsonc
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
                tasks.push(TaskInfo::new(
                    "deno",
                    name.clone(),
                    format!("deno task {name}"),
                    cwd.clone(),
                ));
            }
        }
    }

    // 6. Justfile
    let justfile_exists = ["Justfile", "justfile", ".justfile"]
        .iter()
        .any(|f| current_dir.join(f).exists());
    if source_enabled(source_filter, "just") && justfile_exists {
        match mode {
            TaskDetectionMode::Full => {
                // Try `just --summary`. Keep this out of passive diagnostics because
                // justfiles may invoke shell during evaluation.
                if let Ok(output) = command_output_with_timeout(
                    Path::new("just"),
                    &["--summary"],
                    current_dir,
                    Duration::from_millis(1500),
                ) {
                    let text = String::from_utf8_lossy(&output.stdout);
                    for name in text.split_whitespace() {
                        tasks.push(TaskInfo::new(
                            "just",
                            name,
                            format!("just {name}"),
                            cwd.clone(),
                        ));
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
fn has_gradle_project(path: &Path) -> bool {
    [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "gradlew",
    ]
    .iter()
    .any(|name| path.join(name).exists())
}
pub(super) fn parse_gradle_task_names(output: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        let Some((name, _description)) = trimmed.split_once(" - ") else {
            continue;
        };
        let name = name.trim();
        if is_gradle_task_name(name) {
            names.push(name.to_string());
        }
    }
    dedup_strings(names)
}
fn is_gradle_task_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
}
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
