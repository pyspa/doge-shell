//! `pm status` and its data: provider/trust/lockfile detection (`MiseStatus`), the JSON shape (`ProjectStatusJson`), and the plain-text renderer (`print_project_status`).
use super::*;

pub(super) fn status(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
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
pub(super) struct ProjectStatusJson {
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
pub(super) struct ProjectRuntimeJson {
    name: String,
    source: String,
    version: Option<String>,
    path: String,
}

pub(super) fn build_project_status(
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

pub(super) fn print_provider_status(ctx: &Context, root: &Path) {
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
pub(super) struct MiseStatus {
    pub(super) executable: Option<PathBuf>,
    pub(super) trust: String,
    pub(super) lockfile: Option<String>,
    pub(super) missing_tools: Vec<String>,
}

impl MiseStatus {
    pub(super) fn detect(root: &Path) -> Self {
        Self::detect_with_executable(root, find_executable("mise"))
    }

    pub(super) fn detect_with_executable(root: &Path, executable: Option<PathBuf>) -> Self {
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

pub(super) fn mise_missing_tools(mise: &Path, root: &Path) -> Result<Vec<String>> {
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

pub(super) fn mise_output(mise: &Path, root: &Path, args: &[&str]) -> Result<std::process::Output> {
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

pub(super) fn trust_output_is_trusted(output: &std::process::Output) -> bool {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .any(|line| {
            line.rsplit_once(':')
                .map(|(_, status)| status.trim() == "trusted")
                .unwrap_or_else(|| line.trim() == "trusted")
        })
}

pub(super) fn mise_config_is_safe(root: &Path) -> bool {
    let configs = [root.join("mise.toml"), root.join(".mise.toml")]
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    !configs.is_empty() && configs.iter().all(|path| mise_config_file_is_safe(path))
}

pub(super) fn mise_config_file_is_safe(path: &Path) -> bool {
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

pub(super) fn safe_mise_task_value(value: &toml::Value, key: Option<&str>) -> bool {
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

pub(super) fn collect_missing_tool_names(value: &JsonValue, tools: &mut Vec<String>) {
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

pub(super) fn detect_dev_container(root: &Path) -> Option<String> {
    [
        root.join(".devcontainer/devcontainer.json"),
        root.join(".devcontainer.json"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .map(|path| path.display().to_string())
}

pub(super) fn find_executable(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}
pub(super) fn print_project_status(
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

pub(super) fn task_source_counts(tasks: &[task::TaskInfo]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for task in tasks {
        *counts.entry(task.source.clone()).or_insert(0) += 1;
    }
    counts
}
