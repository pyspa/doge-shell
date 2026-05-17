use super::{DynamicCompletionProvider, dedup_sorted, run_command_lines};
use crate::completion::integrated::EnhancedCandidate;
use dsh_builtin::project_context;
use std::fs;
use std::path::Path;

impl DynamicCompletionProvider {
    pub(crate) fn collect_python_project_dependency_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = project_context::find_project_root(current_dir);
        self.collect_cached_value_candidates(
            "python",
            "project-dependency",
            project_root.clone(),
            current_token,
            "python project dependency",
            cached_only,
            move || Ok(load_python_project_dependencies(&project_root)),
        )
    }

    pub(crate) fn collect_node_bin_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = project_context::find_project_root(current_dir);
        self.collect_cached_value_candidates(
            "node",
            "bin",
            project_root.clone(),
            current_token,
            "node_modules binary",
            cached_only,
            move || Ok(load_node_bin_names(&project_root)),
        )
    }

    pub(crate) fn collect_go_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("go");
        let project_root = project_context::find_project_root(current_dir);
        self.collect_cached_value_candidates(
            "go",
            "package",
            project_root.clone(),
            current_token,
            "go package",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let lines = run_command_lines(
                    &command_path,
                    &["list", "-f", "{{.ImportPath}}\t{{.Dir}}", "./..."],
                    &project_root,
                )?;
                Ok(parse_go_list_package_values(&lines, &project_root))
            },
        )
    }
}

fn load_python_project_dependencies(project_root: &Path) -> Vec<String> {
    let mut values = Vec::new();
    values.extend(load_pyproject_dependencies(
        &project_root.join("pyproject.toml"),
    ));
    values.extend(load_requirement_dependencies(project_root));
    values.extend(load_pipfile_dependencies(&project_root.join("Pipfile")));
    dedup_sorted(values)
}

fn load_pyproject_dependencies(path: &Path) -> Vec<String> {
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

fn toml_array_dependency_names(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter_map(parse_python_dependency_name)
        .collect()
}

fn toml_table_dependency_keys(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|table| table.keys())
        .filter(|key| !key.eq_ignore_ascii_case("python"))
        .filter_map(|key| parse_python_dependency_name(key))
        .collect()
}

fn load_requirement_dependencies(project_root: &Path) -> Vec<String> {
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

fn parse_requirement_line(line: &str) -> Option<String> {
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

fn load_pipfile_dependencies(path: &Path) -> Vec<String> {
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

fn parse_python_dependency_name(value: &str) -> Option<String> {
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

fn load_node_bin_names(project_root: &Path) -> Vec<String> {
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

fn parse_go_list_package_values(lines: &[String], project_root: &Path) -> Vec<String> {
    let mut values = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (import_path, dir) = trimmed
            .split_once('\t')
            .map(|(import_path, dir)| (import_path.trim(), Some(dir.trim())))
            .unwrap_or((trimmed, None));
        if !import_path.is_empty() {
            values.push(import_path.to_string());
        }

        if let Some(dir) = dir {
            values.extend(relative_go_package_value(project_root, Path::new(dir)));
        }
    }

    if !values.is_empty() {
        values.push("./...".to_string());
    }
    dedup_sorted(values)
}

fn relative_go_package_value(project_root: &Path, dir: &Path) -> Option<String> {
    let relative = dir.strip_prefix(project_root).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(".".to_string());
    }
    let parts = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("./{}", parts.join("/")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn python_dependency_parser_reads_common_project_files() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            r#"
[project]
dependencies = ["requests>=2", "fastapi[standard]"]
[project.optional-dependencies]
dev = ["pytest>=8"]
[dependency-groups]
lint = ["ruff==0.8"]
[tool.poetry.dependencies]
python = "^3.12"
pendulum = "^3"
[tool.poetry.group.docs.dependencies]
mkdocs = "^1"
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("requirements-dev.txt"),
            "black==24.0\n-r base.txt\n./local-package\ngit+https://example.invalid/pkg\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Pipfile"),
            "[packages]\nflask = \"*\"\n[dev-packages]\ncoverage = \"*\"\n",
        )
        .unwrap();

        assert_eq!(
            load_python_project_dependencies(dir.path()),
            vec![
                "black".to_string(),
                "coverage".to_string(),
                "fastapi".to_string(),
                "flask".to_string(),
                "mkdocs".to_string(),
                "pendulum".to_string(),
                "pytest".to_string(),
                "requests".to_string(),
                "ruff".to_string(),
            ]
        );
    }

    #[test]
    fn node_bin_loader_reads_local_package_binaries() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("vite"), "").unwrap();
        fs::write(bin_dir.join("eslint"), "").unwrap();
        fs::write(bin_dir.join(".ignored"), "").unwrap();

        assert_eq!(
            load_node_bin_names(dir.path()),
            vec!["eslint".to_string(), "vite".to_string()]
        );
    }

    #[test]
    fn go_list_parser_exposes_import_and_relative_package_values() {
        let root = PathBuf::from("/workspace/app");
        let lines = vec![
            "/workspace/app\t/workspace/app".to_string(),
            "example.com/app/pkg/api\t/workspace/app/pkg/api".to_string(),
        ];

        assert_eq!(
            parse_go_list_package_values(&lines, &root),
            vec![
                ".".to_string(),
                "./...".to_string(),
                "./pkg/api".to_string(),
                "/workspace/app".to_string(),
                "example.com/app/pkg/api".to_string(),
            ]
        );
    }

    #[test]
    fn dynamic_collectors_filter_cached_values_by_prefix() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"requests>=2\", \"pytest\"]\n",
        )
        .unwrap();
        let bin_dir = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("vite"), "").unwrap();

        let provider = DynamicCompletionProvider::new(crate::environment::Environment::new());
        let py = provider.collect_python_project_dependency_candidates(dir.path(), "req", false);
        assert_eq!(py[0].text, "requests");

        let node = provider.collect_node_bin_candidates(dir.path(), "vi", false);
        assert_eq!(node[0].text, "vite");
    }
}
