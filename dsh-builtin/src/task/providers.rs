//! Machine-readable task discovery for ecosystems whose task names aren't declared in a static file `project_context` can just read: mise, nx, and turbo each need their CLI invoked to list tasks.
use super::runtime::TaskDiscoveryRuntime;
use super::*;

trait TaskProvider {
    fn discover(
        &self,
        root: &Path,
        runtime: &TaskDiscoveryRuntime,
    ) -> Result<Vec<project_context::TaskDefinition>>;
}
struct StaticTaskProvider;
impl TaskProvider for StaticTaskProvider {
    fn discover(
        &self,
        root: &Path,
        _runtime: &TaskDiscoveryRuntime,
    ) -> Result<Vec<project_context::TaskDefinition>> {
        project_context::detect_task_names_in_dir(root)
    }
}
struct MiseTaskProvider {
    executable: std::path::PathBuf,
}
struct NxTaskProvider {
    executable: std::path::PathBuf,
}
struct TurboTaskProvider {
    executable: std::path::PathBuf,
}
impl TaskProvider for NxTaskProvider {
    fn discover(
        &self,
        root: &Path,
        runtime: &TaskDiscoveryRuntime,
    ) -> Result<Vec<project_context::TaskDefinition>> {
        let projects_output = command_output_with_timeout(
            &self.executable,
            &["show", "projects", "--json"],
            root,
            Duration::from_millis(1500),
            runtime.child_env(),
        )?;
        if !projects_output.status.success() {
            return Err(anyhow::anyhow!("nx show projects --json failed"));
        }
        let projects: Vec<String> = serde_json::from_slice(&projects_output.stdout)?;
        let mut tasks = Vec::new();
        for project in projects {
            let output = command_output_with_timeout(
                &self.executable,
                &["show", "project", project.as_str(), "--json"],
                root,
                Duration::from_millis(1500),
                runtime.child_env(),
            )?;
            if !output.status.success() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if let Some(targets) = value.get("targets").and_then(serde_json::Value::as_object) {
                for target in targets.keys() {
                    let name = format!("{project}:{target}");
                    tasks.push(project_context::TaskDefinition {
                        source: "nx".to_string(),
                        name: name.clone(),
                        command: format!("nx run {name}"),
                    });
                }
            }
        }
        Ok(tasks)
    }
}
impl TaskProvider for MiseTaskProvider {
    fn discover(
        &self,
        root: &Path,
        runtime: &TaskDiscoveryRuntime,
    ) -> Result<Vec<project_context::TaskDefinition>> {
        let output = command_output_with_timeout(
            &self.executable,
            &["--no-hooks", "tasks", "ls", "--json", "--all", "--local"],
            root,
            Duration::from_millis(1500),
            runtime.child_env(),
        )?;
        if !output.status.success() {
            return Err(anyhow::anyhow!("mise tasks ls --json failed"));
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(parse_mise_tasks_json(&value))
    }
}
impl TaskProvider for TurboTaskProvider {
    fn discover(
        &self,
        root: &Path,
        runtime: &TaskDiscoveryRuntime,
    ) -> Result<Vec<project_context::TaskDefinition>> {
        let names = StaticTaskProvider
            .discover(root, runtime)?
            .into_iter()
            .filter(|task| task.source == "turbo")
            .map(|task| task.name)
            .collect::<Vec<_>>();
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let mut args = vec!["run".to_string()];
        args.extend(names);
        args.push("--dry=json".to_string());
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = command_output_with_timeout(
            &self.executable,
            &arg_refs,
            root,
            Duration::from_millis(1500),
            runtime.child_env(),
        )?;
        if !output.status.success() {
            return Err(anyhow::anyhow!("turbo run --dry=json failed"));
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(parse_turbo_tasks_json(&value))
    }
}
pub(super) fn discover_provider_tasks(
    root: &Path,
    runtime: &TaskDiscoveryRuntime,
) -> Result<Vec<project_context::TaskDefinition>> {
    let mut tasks = StaticTaskProvider.discover(root, runtime)?;
    if (root.join("mise.toml").exists() || root.join(".mise.toml").exists())
        && let Some(executable) = runtime.resolve_program("mise")
    {
        let provider = MiseTaskProvider { executable };
        if let Ok(machine_tasks) = provider.discover(root, runtime)
            && !machine_tasks.is_empty()
        {
            tasks.retain(|task| task.source != "mise");
            tasks.extend(machine_tasks);
        }
    }
    if (root.join("nx.json").exists()
        || root.join("workspace.json").exists()
        || root.join("project.json").exists())
        && let Some(executable) = runtime.resolve_project_program(root, "nx")
    {
        let provider = NxTaskProvider { executable };
        if let Ok(machine_tasks) = provider.discover(root, runtime)
            && !machine_tasks.is_empty()
        {
            tasks.retain(|task| task.source != "nx");
            tasks.extend(machine_tasks);
        }
    }
    if root.join("turbo.json").exists()
        && let Some(executable) = runtime.resolve_project_program(root, "turbo")
    {
        let provider = TurboTaskProvider { executable };
        if let Ok(machine_tasks) = provider.discover(root, runtime)
            && !machine_tasks.is_empty()
        {
            tasks.retain(|task| task.source != "turbo");
            tasks.extend(machine_tasks);
        }
    }
    Ok(tasks)
}
pub(super) fn parse_mise_tasks_json(
    value: &serde_json::Value,
) -> Vec<project_context::TaskDefinition> {
    let mut tasks = Vec::new();
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                if let Some(name) = value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.as_str())
                {
                    tasks.push(project_context::TaskDefinition {
                        source: "mise".to_string(),
                        name: name.to_string(),
                        command: format!("mise run {name}"),
                    });
                }
            }
        }
        serde_json::Value::Object(values) => {
            for name in values.keys() {
                tasks.push(project_context::TaskDefinition {
                    source: "mise".to_string(),
                    name: name.to_string(),
                    command: format!("mise run {name}"),
                });
            }
        }
        _ => {}
    }
    tasks
}
pub(super) fn parse_turbo_tasks_json(
    value: &serde_json::Value,
) -> Vec<project_context::TaskDefinition> {
    let Some(values) = value.get("tasks").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    values
        .iter()
        .filter_map(|value| value.get("task").and_then(serde_json::Value::as_str))
        .filter(|name| seen.insert((*name).to_string()))
        .map(|name| project_context::TaskDefinition {
            source: "turbo".to_string(),
            name: name.to_string(),
            command: format!("turbo run {name}"),
        })
        .collect()
}
