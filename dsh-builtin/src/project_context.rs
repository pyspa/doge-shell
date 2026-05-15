use anyhow::Result;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;

const PROJECT_MARKERS: &[&str] = &[
    "mise.toml",
    ".tool-versions",
    "Cargo.toml",
    "rust-toolchain.toml",
    "rust-toolchain",
    "package.json",
    "turbo.json",
    "project.json",
    "pyproject.toml",
    "requirements.txt",
    "Pipfile",
    ".python-version",
    ".node-version",
    ".nvmrc",
    "go.mod",
    "Taskfile.yml",
    "Taskfile.yaml",
    "deno.json",
    "deno.jsonc",
    "Justfile",
    "justfile",
    ".justfile",
    "Makefile",
    "makefile",
    ".git",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub project_markers: Vec<String>,
    pub runtimes: Vec<RuntimeContext>,
    pub activations: Vec<ActivationContext>,
}

impl ProjectContext {
    pub fn runtime(&self, name: &str) -> Option<&RuntimeContext> {
        self.runtimes.iter().find(|runtime| runtime.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContext {
    pub name: String,
    pub source: String,
    pub version: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationContext {
    pub kind: String,
    pub path: PathBuf,
}

pub fn resolve_project_context(current_dir: &Path) -> ProjectContext {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    let project_root = find_project_root(&cwd);
    let project_markers = detect_project_markers(&project_root);
    let runtimes = detect_runtimes(&cwd, &project_root);
    let activations = detect_activations(&cwd, &project_root);

    ProjectContext {
        cwd,
        project_root,
        project_markers,
        runtimes,
        activations,
    }
}

pub fn find_project_root(current_dir: &Path) -> PathBuf {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    for ancestor in cwd.ancestors() {
        if has_any_marker(ancestor) {
            return ancestor.to_path_buf();
        }
    }
    cwd
}

fn has_any_marker(dir: &Path) -> bool {
    PROJECT_MARKERS
        .iter()
        .any(|marker| dir.join(marker).exists())
}

fn detect_project_markers(project_root: &Path) -> Vec<String> {
    PROJECT_MARKERS
        .iter()
        .filter(|marker| project_root.join(marker).exists())
        .map(|marker| (*marker).to_string())
        .collect()
}

fn detect_runtimes(current_dir: &Path, project_root: &Path) -> Vec<RuntimeContext> {
    let mut runtimes = Vec::new();
    if let Some(runtime) = detect_rust_runtime(current_dir, project_root) {
        runtimes.push(runtime);
    }
    if let Some(runtime) = detect_node_runtime(current_dir, project_root) {
        runtimes.push(runtime);
    }
    if let Some(runtime) = detect_python_runtime(current_dir, project_root) {
        runtimes.push(runtime);
    }
    if let Some(runtime) = detect_go_runtime(current_dir, project_root) {
        runtimes.push(runtime);
    }
    runtimes
}

fn detect_activations(current_dir: &Path, project_root: &Path) -> Vec<ActivationContext> {
    let mut activations = Vec::new();
    for (kind, file_name) in [
        ("envrc", ".envrc"),
        ("dotenv", ".env"),
        ("venv", ".venv"),
        ("venv", "venv"),
    ] {
        if let Some(path) = find_path_upwards(current_dir, file_name)
            && path.starts_with(project_root)
        {
            activations.push(ActivationContext {
                kind: kind.to_string(),
                path,
            });
        }
    }

    dedup_activations(activations)
}

fn detect_rust_runtime(current_dir: &Path, project_root: &Path) -> Option<RuntimeContext> {
    if !(project_root.join("Cargo.toml").exists()
        || find_path_upwards(current_dir, "rust-toolchain.toml").is_some()
        || find_path_upwards(current_dir, "rust-toolchain").is_some())
    {
        return None;
    }

    detect_runtime_from_mise(current_dir, "rust", &["rust"])
        .or_else(|| detect_runtime_from_tool_versions(current_dir, "rust", &["rust"]))
        .or_else(|| detect_runtime_from_rust_toolchain(current_dir))
        .or_else(|| {
            Some(RuntimeContext {
                name: "rust".to_string(),
                source: "Cargo.toml".to_string(),
                version: None,
                path: project_root.join("Cargo.toml"),
            })
        })
}

fn detect_node_runtime(current_dir: &Path, project_root: &Path) -> Option<RuntimeContext> {
    if !(project_root.join("package.json").exists()
        || find_path_upwards(current_dir, ".node-version").is_some()
        || find_path_upwards(current_dir, ".nvmrc").is_some())
    {
        return None;
    }

    detect_runtime_from_mise(current_dir, "node", &["node", "nodejs"])
        .or_else(|| detect_runtime_from_tool_versions(current_dir, "node", &["node", "nodejs"]))
        .or_else(|| detect_runtime_from_text_file(current_dir, "node", ".node-version"))
        .or_else(|| detect_runtime_from_text_file(current_dir, "node", ".nvmrc"))
        .or_else(|| {
            Some(RuntimeContext {
                name: "node".to_string(),
                source: "package.json".to_string(),
                version: None,
                path: project_root.join("package.json"),
            })
        })
}

fn detect_python_runtime(current_dir: &Path, project_root: &Path) -> Option<RuntimeContext> {
    let has_python_project = project_root.join("pyproject.toml").exists()
        || project_root.join("requirements.txt").exists()
        || project_root.join("Pipfile").exists()
        || project_root.join(".venv").exists()
        || project_root.join("venv").exists()
        || find_path_upwards(current_dir, ".python-version").is_some();
    if !has_python_project {
        return None;
    }

    detect_runtime_from_mise(current_dir, "python", &["python"])
        .or_else(|| detect_runtime_from_tool_versions(current_dir, "python", &["python"]))
        .or_else(|| detect_runtime_from_text_file(current_dir, "python", ".python-version"))
        .or_else(|| detect_runtime_from_directory(current_dir, "python", ".venv"))
        .or_else(|| detect_runtime_from_directory(current_dir, "python", "venv"))
        .or_else(|| {
            let fallback = ["pyproject.toml", "requirements.txt", "Pipfile"]
                .into_iter()
                .find_map(|name| {
                    let path = project_root.join(name);
                    path.exists().then_some((name, path))
                });
            fallback.map(|(name, path)| RuntimeContext {
                name: "python".to_string(),
                source: name.to_string(),
                version: None,
                path,
            })
        })
}

fn detect_go_runtime(current_dir: &Path, project_root: &Path) -> Option<RuntimeContext> {
    if !(project_root.join("go.mod").exists() || find_path_upwards(current_dir, "go.mod").is_some())
    {
        return None;
    }

    detect_runtime_from_mise(current_dir, "go", &["go"])
        .or_else(|| detect_runtime_from_tool_versions(current_dir, "go", &["go", "golang"]))
        .or_else(|| {
            project_root
                .join("go.mod")
                .exists()
                .then(|| RuntimeContext {
                    name: "go".to_string(),
                    source: "go.mod".to_string(),
                    version: None,
                    path: project_root.join("go.mod"),
                })
        })
}

fn detect_runtime_from_directory(
    current_dir: &Path,
    runtime_name: &str,
    dir_name: &str,
) -> Option<RuntimeContext> {
    let path = find_path_upwards(current_dir, dir_name)?;
    path.is_dir().then(|| RuntimeContext {
        name: runtime_name.to_string(),
        source: dir_name.to_string(),
        version: None,
        path,
    })
}

fn detect_runtime_from_text_file(
    current_dir: &Path,
    runtime_name: &str,
    file_name: &str,
) -> Option<RuntimeContext> {
    let path = find_path_upwards(current_dir, file_name)?;
    let version = read_trimmed_file(&path)?;
    Some(RuntimeContext {
        name: runtime_name.to_string(),
        source: file_name.to_string(),
        version: Some(version),
        path,
    })
}

fn detect_runtime_from_tool_versions(
    current_dir: &Path,
    runtime_name: &str,
    tool_names: &[&str],
) -> Option<RuntimeContext> {
    let path = find_path_upwards(current_dir, ".tool-versions")?;
    let content = fs::read_to_string(&path).ok()?;
    let version = parse_tool_versions(&content, tool_names)?;
    Some(RuntimeContext {
        name: runtime_name.to_string(),
        source: ".tool-versions".to_string(),
        version: Some(version),
        path,
    })
}

fn detect_runtime_from_mise(
    current_dir: &Path,
    runtime_name: &str,
    tool_names: &[&str],
) -> Option<RuntimeContext> {
    let path = find_path_upwards(current_dir, "mise.toml")?;
    let content = fs::read_to_string(&path).ok()?;
    let version = parse_mise_tool_version(&content, tool_names)?;
    Some(RuntimeContext {
        name: runtime_name.to_string(),
        source: "mise".to_string(),
        version: Some(version),
        path,
    })
}

fn detect_runtime_from_rust_toolchain(current_dir: &Path) -> Option<RuntimeContext> {
    if let Some(path) = find_path_upwards(current_dir, "rust-toolchain.toml") {
        let content = fs::read_to_string(&path).ok()?;
        let value = toml::from_str::<TomlValue>(&content).ok()?;
        let version = value
            .get("toolchain")
            .and_then(TomlValue::as_table)
            .and_then(|table| table.get("channel"))
            .and_then(TomlValue::as_str)
            .map(str::to_string)?;
        return Some(RuntimeContext {
            name: "rust".to_string(),
            source: "rust-toolchain.toml".to_string(),
            version: Some(version),
            path,
        });
    }

    let path = find_path_upwards(current_dir, "rust-toolchain")?;
    let version = read_trimmed_file(&path)?;
    Some(RuntimeContext {
        name: "rust".to_string(),
        source: "rust-toolchain".to_string(),
        version: Some(version),
        path,
    })
}

fn parse_tool_versions(content: &str, tool_names: &[&str]) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let tool = parts.next()?;
        if tool_names.iter().any(|candidate| candidate == &tool) {
            let version = parts.next()?.trim();
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
    }
    None
}

fn parse_mise_tool_version(content: &str, tool_names: &[&str]) -> Option<String> {
    let value = toml::from_str::<TomlValue>(content).ok()?;
    let tools = value.get("tools")?.as_table()?;

    for tool_name in tool_names {
        let Some(value) = tools.get(*tool_name) else {
            continue;
        };
        if let Some(version) = extract_version_from_toml(value) {
            return Some(version);
        }
    }

    None
}

fn extract_version_from_toml(value: &TomlValue) -> Option<String> {
    match value {
        TomlValue::String(version) => Some(version.clone()),
        TomlValue::Table(table) => table
            .get("version")
            .and_then(TomlValue::as_str)
            .map(str::to_string),
        TomlValue::Array(values) => values.iter().find_map(extract_version_from_toml),
        _ => None,
    }
}

fn find_path_upwards(current_dir: &Path, file_name: &str) -> Option<PathBuf> {
    for ancestor in current_dir.ancestors() {
        let candidate = ancestor.join(file_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn read_trimmed_file(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
}

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
    let path = root.join("mise.toml");
    if !path.exists() {
        return Ok(Vec::new());
    }

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

fn detect_nx_tasks(root: &Path) -> Result<Vec<TaskDefinition>> {
    let path = root.join("project.json");
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)?;
    let value = serde_json::from_str::<JsonValue>(&content)?;
    let project_name = value
        .get("name")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        });
    let Some(project_name) = project_name else {
        return Ok(Vec::new());
    };

    let mut tasks = Vec::new();
    if let Some(targets) = value.get("targets").and_then(JsonValue::as_object) {
        for name in targets.keys() {
            tasks.push(TaskDefinition {
                source: "nx".to_string(),
                name: name.clone(),
                command: format!("nx run {}:{}", project_name, name),
            });
        }
    }
    Ok(tasks)
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

fn dedup_activations(activations: Vec<ActivationContext>) -> Vec<ActivationContext> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for activation in activations {
        let key = (
            activation.kind.clone(),
            activation.path.to_string_lossy().to_string(),
        );
        if seen.insert(key) {
            deduped.push(activation);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolves_project_root_and_runtime_sources_from_parent() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("mise.toml"), "[tools]\nnode = '20.11.0'\n").unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
        let nested = dir.path().join("apps").join("web").join("src");
        fs::create_dir_all(&nested).unwrap();

        let context = resolve_project_context(&nested);
        assert_eq!(context.project_root, dir.path().canonicalize().unwrap());
        assert!(
            context
                .project_markers
                .iter()
                .any(|marker| marker == "mise.toml")
        );

        let node = context.runtime("node").unwrap();
        assert_eq!(node.source, "mise");
        assert_eq!(node.version.as_deref(), Some("20.11.0"));
    }

    #[test]
    fn prefers_language_version_file_when_mise_missing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
        fs::write(dir.path().join(".nvmrc"), "22.0.0\n").unwrap();

        let context = resolve_project_context(dir.path());
        let node = context.runtime("node").unwrap();
        assert_eq!(node.source, ".nvmrc");
        assert_eq!(node.version.as_deref(), Some("22.0.0"));
    }

    #[test]
    fn detects_task_sources_from_project_root() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("mise.toml"),
            "[tasks.build]\nrun = 'cargo build'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  lint:\n    cmds:\n      - cargo clippy\n",
        )
        .unwrap();
        fs::write(dir.path().join("turbo.json"), "{\"tasks\":{\"dev\":{}}}").unwrap();
        fs::write(
            dir.path().join("project.json"),
            "{\"name\":\"web\",\"targets\":{\"test\":{}}}",
        )
        .unwrap();
        let nested = dir.path().join("src");
        fs::create_dir_all(&nested).unwrap();

        let tasks = detect_task_names_in_dir(&nested).unwrap();
        assert!(
            tasks
                .iter()
                .any(|task| task.source == "mise" && task.name == "build")
        );
        assert!(
            tasks
                .iter()
                .any(|task| task.source == "taskfile" && task.name == "lint")
        );
        assert!(
            tasks
                .iter()
                .any(|task| task.source == "turbo" && task.name == "dev")
        );
        assert!(
            tasks
                .iter()
                .any(|task| task.source == "nx" && task.name == "test")
        );
    }

    #[test]
    fn treats_deno_config_as_project_root_marker() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("deno.json"),
            "{\"tasks\":{\"build\":\"deno run build.ts\"}}",
        )
        .unwrap();
        let nested = dir.path().join("src").join("cli");
        fs::create_dir_all(&nested).unwrap();

        let context = resolve_project_context(&nested);
        assert_eq!(context.project_root, dir.path().canonicalize().unwrap());
        assert!(
            context
                .project_markers
                .iter()
                .any(|marker| marker == "deno.json")
        );
    }

    #[test]
    fn parses_tool_versions_for_multiple_runtime_names() {
        let content = "nodejs 20.10.0\ngolang 1.22.2\n";
        assert_eq!(
            parse_tool_versions(content, &["node", "nodejs"]).as_deref(),
            Some("20.10.0")
        );
        assert_eq!(
            parse_tool_versions(content, &["go", "golang"]).as_deref(),
            Some("1.22.2")
        );
    }

    #[test]
    fn detects_python_venv_activation() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname='demo'\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join(".venv")).unwrap();
        let nested = dir.path().join("pkg");
        fs::create_dir_all(&nested).unwrap();

        let context = resolve_project_context(&nested);
        assert!(
            context
                .activations
                .iter()
                .any(|activation| activation.kind == "venv")
        );
        let python = context.runtime("python").unwrap();
        assert_eq!(python.source, ".venv");
    }
}
