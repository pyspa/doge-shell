//! Node.js workspace and `node_modules/.bin` discovery, including `npm`/
//! `pnpm`/`yarn` workspace glob expansion.
use super::*;

pub(super) fn find_node_bin_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| ancestor.join("node_modules").join(".bin").is_dir())
        .map(Path::to_path_buf)
}

pub(super) fn find_node_workspace_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| {
            ancestor.join("pnpm-workspace.yaml").is_file()
                || package_json_has_workspaces(&ancestor.join("package.json"))
        })
        .map(Path::to_path_buf)
}

pub(super) fn package_json_has_workspaces(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };
    match value.get("workspaces") {
        Some(serde_json::Value::Array(values)) => !values.is_empty(),
        Some(serde_json::Value::Object(object)) => object
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|values| !values.is_empty()),
        _ => false,
    }
}

pub(super) fn load_node_bin_names(project_root: &Path) -> Vec<String> {
    let bin_dir = project_root.join("node_modules").join(".bin");
    let Ok(entries) = fs::read_dir(bin_dir) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| !name.is_empty() && !name.starts_with('.'))
            .collect(),
    )
}

pub(super) fn load_node_workspaces(project_root: &Path) -> Vec<String> {
    let mut patterns = Vec::new();
    patterns.extend(load_package_json_workspace_patterns(
        &project_root.join("package.json"),
    ));
    patterns.extend(load_pnpm_workspace_patterns(
        &project_root.join("pnpm-workspace.yaml"),
    ));

    let mut values = Vec::new();
    for pattern in patterns {
        values.extend(expand_node_workspace_pattern(project_root, &pattern));
    }
    dedup_sorted(values)
}

pub(super) fn load_package_json_workspace_patterns(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };
    let Some(workspaces) = value.get("workspaces") else {
        return Vec::new();
    };

    if let Some(array) = workspaces.as_array() {
        return array
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(clean_workspace_pattern)
            .collect();
    }

    workspaces
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(clean_workspace_pattern)
        .collect()
}

pub(super) fn load_pnpm_workspace_patterns(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut in_packages = false;
    let mut patterns = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !in_packages {
            in_packages = trimmed == "packages:";
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        if let Some(value) = trimmed.strip_prefix('-')
            && let Some(pattern) = clean_workspace_pattern(value.trim())
        {
            patterns.push(pattern);
        }
    }
    patterns
}

pub(super) fn clean_workspace_pattern(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '\'']);
    if value.is_empty() || value.starts_with('!') || value.contains("://") || value.starts_with('/')
    {
        None
    } else {
        Some(value.to_string())
    }
}

pub(super) fn expand_node_workspace_pattern(project_root: &Path, pattern: &str) -> Vec<String> {
    let mut values = Vec::new();
    if !pattern.contains('*') {
        let path = project_root.join(pattern);
        if path.is_dir() {
            values.extend(node_workspace_values_for_dir(project_root, &path));
        }
        return values;
    }

    let glob_pattern = project_root.join(pattern).to_string_lossy().to_string();
    let Ok(paths) = glob::glob(&glob_pattern) else {
        return Vec::new();
    };
    for path in paths.flatten().filter(|path| path.is_dir()) {
        values.extend(node_workspace_values_for_dir(project_root, &path));
    }
    values
}

pub(super) fn node_workspace_values_for_dir(
    project_root: &Path,
    workspace_dir: &Path,
) -> Vec<String> {
    let mut values = Vec::new();
    if let Ok(relative) = workspace_dir.strip_prefix(project_root)
        && let Some(value) = relative.to_str()
        && !value.is_empty()
    {
        values.push(value.replace('\\', "/"));
    }

    let package_json = workspace_dir.join("package.json");
    if let Ok(contents) = fs::read_to_string(package_json)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents)
        && let Some(name) = value.get("name").and_then(serde_json::Value::as_str)
        && !name.trim().is_empty()
    {
        values.push(name.trim().to_string());
    }
    values
}
