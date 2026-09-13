use anyhow::Result;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;

mod runtime;
mod tasks;
#[cfg(test)]
use runtime::parse_tool_versions;
use runtime::{
    detect_go_runtime, detect_node_runtime, detect_python_runtime, detect_rust_runtime,
    find_path_upwards,
};
#[cfg(test)]
use tasks::detect_nx_tasks;
pub use tasks::{TaskDefinition, detect_task_names_in_dir};

const PROJECT_MARKERS: &[&str] = &[
    "mise.toml",
    ".mise.toml",
    ".tool-versions",
    "Cargo.toml",
    "rust-toolchain.toml",
    "rust-toolchain",
    "package.json",
    "workspace.json",
    "angular.json",
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

/// Whether `dir` looks like the root of a project.
///
/// Exposed so the chat tools can widen their sandbox from a workspace member
/// out to the workspace itself.
pub fn has_project_marker(dir: &Path) -> bool {
    has_any_marker(dir)
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
                .any(|task| task.source == "nx" && task.name.ends_with(":test"))
        );
    }

    #[test]
    fn detects_nx_tasks_from_workspace_and_descendant_project_json() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("workspace.json"),
            r#"{
              "projects": {
                "web": { "targets": { "build": {}, "test": {} } },
                "legacy": { "architect": { "serve": {} } }
              }
            }"#,
        )
        .unwrap();
        let api_dir = dir.path().join("apps").join("api");
        fs::create_dir_all(&api_dir).unwrap();
        fs::write(
            api_dir.join("project.json"),
            r#"{ "name": "api", "targets": { "lint": {} } }"#,
        )
        .unwrap();

        let tasks = detect_nx_tasks(dir.path()).unwrap();
        assert!(tasks.iter().any(|task| {
            task.source == "nx" && task.name == "web:build" && task.command == "nx run web:build"
        }));
        assert!(tasks.iter().any(|task| {
            task.source == "nx"
                && task.name == "legacy:serve"
                && task.command == "nx run legacy:serve"
        }));
        assert!(tasks.iter().any(|task| {
            task.source == "nx" && task.name == "api:lint" && task.command == "nx run api:lint"
        }));

        let detected = detect_task_names_in_dir(dir.path()).unwrap();
        assert!(detected.iter().any(|task| {
            task.source == "nx" && task.name == "web:build" && task.command == "nx run web:build"
        }));
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
