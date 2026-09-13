//! Working out which language runtime version a directory asks for: the
//! per-language probes (Cargo/package.json/pyproject/go.mod) and the shared
//! readers for `.tool-versions`, `mise.toml`, `rust-toolchain.toml` and the
//! plain `.<lang>-version` files, each reported with the file it came from.
use super::*;

pub(super) fn detect_rust_runtime(
    current_dir: &Path,
    project_root: &Path,
) -> Option<RuntimeContext> {
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

pub(super) fn detect_node_runtime(
    current_dir: &Path,
    project_root: &Path,
) -> Option<RuntimeContext> {
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

pub(super) fn detect_python_runtime(
    current_dir: &Path,
    project_root: &Path,
) -> Option<RuntimeContext> {
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

pub(super) fn detect_go_runtime(current_dir: &Path, project_root: &Path) -> Option<RuntimeContext> {
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

pub(super) fn parse_tool_versions(content: &str, tool_names: &[&str]) -> Option<String> {
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

pub(super) fn find_path_upwards(current_dir: &Path, file_name: &str) -> Option<PathBuf> {
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
