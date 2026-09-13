//! Python tooling: `nox`/`tox`/`hatch`/`pre-commit` session and hook-id
//! discovery (each reading its own config file format), and project/module
//! dependency resolution (`pyproject.toml`, `requirements*.txt`, `Pipfile`,
//! and importable module names under the project root).
use super::*;

pub(super) fn load_nox_sessions(noxfile: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(noxfile) else {
        return Vec::new();
    };
    parse_nox_sessions(&contents)
}

/// Extracts session names from a `noxfile.py`.
///
/// `nox --list-sessions` would report them authoritatively, but it imports and
/// evaluates the noxfile, which would run arbitrary project code just because
/// TAB was pressed. The file is therefore scanned for `@nox.session` /
/// `@session` decorators and the function they decorate, honouring an explicit
/// `name=` override on the decorator.
pub(super) fn parse_nox_sessions(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut pending_session = false;
    let mut pending_name = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("@nox.session") || trimmed.starts_with("@session") {
            pending_session = true;
        }
        if pending_session && pending_name.is_none() {
            pending_name = decorator_name_argument(trimmed);
        }
        if !pending_session {
            continue;
        }
        let Some(rest) = trimmed
            .strip_prefix("def ")
            .or_else(|| trimmed.strip_prefix("async def "))
        else {
            continue;
        };
        let name = pending_name
            .take()
            .or_else(|| rest.split('(').next().map(|name| name.trim().to_string()));
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            values.push(name);
        }
        pending_session = false;
    }
    dedup_sorted(values)
}

/// Reads the `name="..."` keyword out of a decorator line, if present.
pub(super) fn decorator_name_argument(line: &str) -> Option<String> {
    let rest = line.split_once("name=")?.1.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let value = rest[quote.len_utf8()..].split(quote).next()?;
    (!value.is_empty()).then(|| value.to_string())
}

pub(super) fn load_tox_environments(project_root: &Path) -> Vec<String> {
    let mut values = parse_tox_ini_environments(
        &fs::read_to_string(project_root.join("tox.ini")).unwrap_or_default(),
    );
    values.extend(load_toml_table_keys(
        &project_root.join("pyproject.toml"),
        &["tool", "tox", "env"],
    ));
    dedup_sorted(values)
}

/// Extracts environment names from a `tox.ini`: both the `[testenv:NAME]`
/// section headers and the entries of the top level `envlist`.
pub(super) fn parse_tox_ini_environments(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut in_envlist = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_envlist = false;
            if let Some(name) = trimmed
                .strip_prefix("[testenv:")
                .and_then(|rest| rest.strip_suffix(']'))
                && !name.is_empty()
            {
                values.push(name.to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("envlist") {
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            in_envlist = true;
            values.extend(split_tox_envlist(rest));
            continue;
        }
        if in_envlist {
            if trimmed.is_empty() || !line.starts_with(char::is_whitespace) {
                in_envlist = false;
                continue;
            }
            values.extend(split_tox_envlist(trimmed));
        }
    }
    dedup_sorted(values)
}

pub(super) fn split_tox_envlist(value: &str) -> Vec<String> {
    value
        .split_once('#')
        .map_or(value, |(before_comment, _)| before_comment)
        .split([',', ' '])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) fn load_hatch_environments(project_root: &Path) -> Vec<String> {
    let mut values = load_toml_table_keys(
        &project_root.join("pyproject.toml"),
        &["tool", "hatch", "envs"],
    );
    values.extend(load_toml_table_keys(
        &project_root.join("hatch.toml"),
        &["envs"],
    ));
    dedup_sorted(values)
}

pub(super) fn load_toml_table_keys(path: &Path, table_path: &[&str]) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return Vec::new();
    };
    let mut current = &value;
    for key in table_path {
        let Some(next) = current.get(key) else {
            return Vec::new();
        };
        current = next;
    }
    current
        .as_table()
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default()
}

pub(super) fn load_pre_commit_hook_ids(project_root: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(project_root.join(".pre-commit-config.yaml")) else {
        return Vec::new();
    };
    parse_pre_commit_hook_ids(&contents)
}

/// Extracts `id:` values from `.pre-commit-config.yaml`. The file is scanned
/// line by line rather than parsed as YAML so that no extra dependency is
/// needed and partially written configs still yield candidates.
pub(super) fn parse_pre_commit_hook_ids(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        let Some(value) = trimmed.strip_prefix("id:") else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        if !value.is_empty() {
            values.push(value.to_string());
        }
    }
    dedup_sorted(values)
}

pub(super) fn find_nox_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &["noxfile.py"])
}

pub(super) fn find_tox_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &["tox.ini", "pyproject.toml"])
}

pub(super) fn find_hatch_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &["hatch.toml", "pyproject.toml"])
}

pub(super) fn find_pre_commit_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &[".pre-commit-config.yaml"])
}

pub(super) fn load_python_project_dependencies(project_root: &Path) -> Vec<String> {
    let mut values = Vec::new();
    values.extend(load_pyproject_dependencies(
        &project_root.join("pyproject.toml"),
    ));
    values.extend(load_requirement_dependencies(project_root));
    values.extend(load_pipfile_dependencies(&project_root.join("Pipfile")));
    dedup_sorted(values)
}

pub(super) fn load_python_modules(project_root: &Path) -> Vec<String> {
    let mut values = Vec::new();
    values.extend(
        load_python_project_dependencies(project_root)
            .into_iter()
            .filter_map(|name| normalize_python_module_name(&name)),
    );

    for base in [project_root.to_path_buf(), project_root.join("src")] {
        values.extend(load_python_modules_from_dir(&base));
    }
    dedup_sorted(values)
}

pub(super) fn load_python_modules_from_dir(base: &Path) -> Vec<String> {
    let mut values = Vec::new();
    collect_python_modules_from_dir(base, "", 0, &mut values);
    values
}

pub(super) fn collect_python_modules_from_dir(
    base: &Path,
    prefix: &str,
    depth: usize,
    values: &mut Vec<String>,
) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if should_skip_python_module_entry(name) {
            continue;
        }

        if path.is_file() {
            if path.extension().and_then(|ext| ext.to_str()) == Some("py") {
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("");
                if stem != "__init__"
                    && is_python_identifier(stem)
                    && let Some(module) = dotted_name(prefix, stem)
                {
                    values.push(module);
                }
            }
            continue;
        }

        if path.is_dir()
            && path.join("__init__.py").exists()
            && is_python_identifier(name)
            && let Some(module) = dotted_name(prefix, name)
        {
            values.push(module.clone());
            collect_python_modules_from_dir(&path, &module, depth + 1, values);
        }
    }
}

pub(super) fn should_skip_python_module_entry(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "__pycache__" | "node_modules" | "target" | "dist" | "build" | ".venv" | "venv" | "env"
        )
}

pub(super) fn dotted_name(prefix: &str, name: &str) -> Option<String> {
    if prefix.is_empty() {
        Some(name.to_string())
    } else {
        Some(format!("{prefix}.{name}"))
    }
}

pub(super) fn load_pyproject_dependencies(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return Vec::new();
    };

    let mut values = Vec::new();
    if let Some(project) = value.get("project") {
        values.extend(toml_array_dependency_names(project.get("dependencies")));
        if let Some(optional) = project
            .get("optional-dependencies")
            .and_then(toml::Value::as_table)
        {
            for dependencies in optional.values() {
                values.extend(toml_array_dependency_names(Some(dependencies)));
            }
        }
    }

    if let Some(groups) = value
        .get("dependency-groups")
        .and_then(toml::Value::as_table)
    {
        for dependencies in groups.values() {
            values.extend(toml_array_dependency_names(Some(dependencies)));
        }
    }

    if let Some(tool) = value.get("tool") {
        if let Some(uv) = tool.get("uv") {
            values.extend(toml_array_dependency_names(uv.get("dev-dependencies")));
        }

        if let Some(poetry) = tool.get("poetry") {
            values.extend(toml_table_dependency_keys(poetry.get("dependencies")));
            values.extend(toml_table_dependency_keys(poetry.get("dev-dependencies")));
            if let Some(groups) = poetry.get("group").and_then(toml::Value::as_table) {
                for group in groups.values() {
                    values.extend(toml_table_dependency_keys(group.get("dependencies")));
                }
            }
        }
    }

    dedup_sorted(values)
}

pub(super) fn toml_array_dependency_names(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter_map(parse_python_dependency_name)
        .collect()
}

pub(super) fn toml_table_dependency_keys(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|table| table.keys())
        .filter(|key| !key.eq_ignore_ascii_case("python"))
        .filter_map(|key| parse_python_dependency_name(key))
        .collect()
}

pub(super) fn load_requirement_dependencies(project_root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(project_root) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(file_name.starts_with("requirements") && file_name.ends_with(".txt")) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        values.extend(contents.lines().filter_map(parse_requirement_line));
    }
    values
}

pub(super) fn parse_requirement_line(line: &str) -> Option<String> {
    let trimmed = line.split('#').next().unwrap_or("").trim();
    if trimmed.is_empty()
        || trimmed.starts_with('-')
        || trimmed.starts_with('.')
        || trimmed.starts_with("git+")
        || trimmed.contains("://")
    {
        return None;
    }
    parse_python_dependency_name(trimmed)
}

pub(super) fn load_pipfile_dependencies(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return Vec::new();
    };
    let mut values = toml_table_dependency_keys(value.get("packages"));
    values.extend(toml_table_dependency_keys(value.get("dev-packages")));
    values
}

pub(super) fn parse_python_dependency_name(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '\'']);
    if value.is_empty() {
        return None;
    }

    let value = value.split(';').next().unwrap_or(value).trim();
    let value = value.split('[').next().unwrap_or(value).trim();
    let name = value
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>();
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name)
    }
}

pub(super) fn normalize_python_module_name(value: &str) -> Option<String> {
    let name = value.replace('-', "_");
    is_python_dotted_name(&name).then_some(name)
}

pub(super) fn is_python_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(super) fn is_python_dotted_name(value: &str) -> bool {
    value.split('.').all(is_python_identifier)
}
