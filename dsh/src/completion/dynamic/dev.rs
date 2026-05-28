use super::{DynamicCompletionProvider, dedup_sorted, run_command_lines};
use crate::completion::integrated::EnhancedCandidate;
use crate::completion::parser::ParsedCommandLine;
use crate::completion::shell_path::normalize_path_token;
use dsh_builtin::project_context;
use std::fs;
use std::path::{Path, PathBuf};

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
        let bin_root = find_node_bin_root(current_dir).unwrap_or_else(|| project_root.clone());
        self.collect_cached_value_candidates(
            "node",
            "bin",
            bin_root.clone(),
            current_token,
            "node_modules binary",
            cached_only,
            move || Ok(load_node_bin_names(&bin_root)),
        )
    }

    pub(crate) fn collect_node_workspace_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = find_node_workspace_root(current_dir)
            .unwrap_or_else(|| project_context::find_project_root(current_dir));
        self.collect_cached_value_candidates(
            "node",
            "workspace",
            project_root.clone(),
            current_token,
            "node workspace",
            cached_only,
            move || Ok(load_node_workspaces(&project_root)),
        )
    }

    pub(crate) fn collect_python_module_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = project_context::find_project_root(current_dir);
        self.collect_cached_value_candidates(
            "python",
            "module",
            project_root.clone(),
            current_token,
            "python module",
            cached_only,
            move || Ok(load_python_modules(&project_root)),
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

    pub(crate) fn collect_aws_profile_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let aws_dir = aws_config_dir(&self.env_var("HOME"));
        let config_file = self
            .env_var("AWS_CONFIG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| aws_dir.join("config"));
        let credentials_file = self
            .env_var("AWS_SHARED_CREDENTIALS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| aws_dir.join("credentials"));
        self.collect_cached_value_candidates(
            "aws",
            "profile",
            aws_dir,
            current_token,
            "AWS profile",
            cached_only,
            move || Ok(load_aws_profiles(&config_file, &credentials_file)),
        )
    }

    pub(crate) fn collect_gcloud_configuration_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let config_dir = gcloud_config_dir(&self.env_var("HOME"), self.env_var("CLOUDSDK_CONFIG"));
        self.collect_cached_value_candidates(
            "gcloud",
            "configuration",
            config_dir.clone(),
            current_token,
            "gcloud configuration",
            cached_only,
            move || Ok(load_gcloud_configurations(&config_dir)),
        )
    }

    pub(crate) fn collect_gcloud_project_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let config_dir = gcloud_config_dir(&self.env_var("HOME"), self.env_var("CLOUDSDK_CONFIG"));
        self.collect_cached_value_candidates(
            "gcloud",
            "project",
            config_dir.clone(),
            current_token,
            "gcloud project",
            cached_only,
            move || Ok(load_gcloud_projects(&config_dir)),
        )
    }

    pub(crate) fn collect_az_subscription_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let config_dir = azure_config_dir(&self.env_var("HOME"), self.env_var("AZURE_CONFIG_DIR"));
        let profile_file = config_dir.join("azureProfile.json");
        self.collect_cached_value_candidates(
            "az",
            "subscription",
            config_dir,
            current_token,
            "Azure subscription",
            cached_only,
            move || Ok(load_az_subscriptions(&profile_file)),
        )
    }

    pub(crate) fn collect_maven_profile_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let maven_root = find_maven_root(current_dir)
            .unwrap_or_else(|| project_context::find_project_root(current_dir));
        self.collect_cached_value_candidates(
            "maven",
            "profile",
            maven_root.clone(),
            current_token,
            "Maven profile",
            cached_only,
            move || Ok(load_maven_profiles(&maven_root.join("pom.xml"))),
        )
    }

    pub(crate) fn collect_maven_module_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let maven_root = find_maven_root(current_dir)
            .unwrap_or_else(|| project_context::find_project_root(current_dir));
        self.collect_cached_value_candidates(
            "maven",
            "module",
            maven_root.clone(),
            current_token,
            "Maven module",
            cached_only,
            move || Ok(load_maven_modules(&maven_root.join("pom.xml"))),
        )
    }

    pub(crate) fn collect_ansible_inventory_host_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = project_context::find_project_root(current_dir);
        let inventory_paths = selected_ansible_inventory_paths(
            parsed_command_line,
            current_dir,
            project_root.as_path(),
        );
        let value_kind = format!(
            "inventory-host:{}",
            inventory_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(":")
        );
        self.collect_cached_value_candidates(
            "ansible",
            &value_kind,
            project_root,
            current_token,
            "Ansible inventory host/group",
            cached_only,
            move || Ok(load_ansible_inventory_values(&inventory_paths)),
        )
    }

    pub(crate) fn collect_terraform_workspace_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let terraform_root = find_terraform_root(current_dir)
            .unwrap_or_else(|| project_context::find_project_root(current_dir));
        self.collect_cached_value_candidates(
            "terraform",
            "workspace",
            terraform_root.clone(),
            current_token,
            "Terraform workspace",
            cached_only,
            move || Ok(load_terraform_workspaces(&terraform_root)),
        )
    }

    fn env_var(&self, key: &str) -> Option<String> {
        self.environment
            .read()
            .get_var(key)
            .or_else(|| std::env::var(key).ok())
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

fn load_python_modules(project_root: &Path) -> Vec<String> {
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

fn load_python_modules_from_dir(base: &Path) -> Vec<String> {
    let mut values = Vec::new();
    collect_python_modules_from_dir(base, "", 0, &mut values);
    values
}

fn collect_python_modules_from_dir(
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

fn should_skip_python_module_entry(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "__pycache__" | "node_modules" | "target" | "dist" | "build" | ".venv" | "venv" | "env"
        )
}

fn dotted_name(prefix: &str, name: &str) -> Option<String> {
    if prefix.is_empty() {
        Some(name.to_string())
    } else {
        Some(format!("{prefix}.{name}"))
    }
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

fn normalize_python_module_name(value: &str) -> Option<String> {
    let name = value.replace('-', "_");
    is_python_dotted_name(&name).then_some(name)
}

fn is_python_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_python_dotted_name(value: &str) -> bool {
    value.split('.').all(is_python_identifier)
}

fn find_node_bin_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| ancestor.join("node_modules").join(".bin").is_dir())
        .map(Path::to_path_buf)
}

fn find_node_workspace_root(current_dir: &Path) -> Option<PathBuf> {
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

fn package_json_has_workspaces(path: &Path) -> bool {
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

fn load_node_workspaces(project_root: &Path) -> Vec<String> {
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

fn load_package_json_workspace_patterns(path: &Path) -> Vec<String> {
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

fn load_pnpm_workspace_patterns(path: &Path) -> Vec<String> {
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

fn clean_workspace_pattern(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '\'']);
    if value.is_empty() || value.starts_with('!') || value.contains("://") || value.starts_with('/')
    {
        None
    } else {
        Some(value.to_string())
    }
}

fn expand_node_workspace_pattern(project_root: &Path, pattern: &str) -> Vec<String> {
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

fn node_workspace_values_for_dir(project_root: &Path, workspace_dir: &Path) -> Vec<String> {
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

fn aws_config_dir(home: &Option<String>) -> PathBuf {
    home.as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aws")
}

fn load_aws_profiles(config_file: &Path, credentials_file: &Path) -> Vec<String> {
    let mut values = Vec::new();
    values.extend(load_aws_profile_sections(config_file, true));
    values.extend(load_aws_profile_sections(credentials_file, false));
    dedup_sorted(values)
}

fn load_aws_profile_sections(path: &Path, config_style: bool) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(parse_ini_section_name)
        .filter_map(|section| {
            if config_style {
                section
                    .strip_prefix("profile ")
                    .map(str::to_string)
                    .or_else(|| (section == "default").then_some(section))
            } else {
                Some(section)
            }
        })
        .collect()
}

fn parse_ini_section_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let section = trimmed.strip_prefix('[')?.strip_suffix(']')?.trim();
    (!section.is_empty()).then_some(section.to_string())
}

fn gcloud_config_dir(home: &Option<String>, explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| {
            home.as_ref()
                .map(|home| PathBuf::from(home).join(".config/gcloud"))
        })
        .unwrap_or_else(|| PathBuf::from(".config/gcloud"))
}

fn load_gcloud_configurations(config_dir: &Path) -> Vec<String> {
    let configurations_dir = config_dir.join("configurations");
    let Ok(entries) = fs::read_dir(configurations_dir) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter_map(|name| name.strip_prefix("config_").map(str::to_string))
            .collect(),
    )
}

fn load_gcloud_projects(config_dir: &Path) -> Vec<String> {
    let mut values = Vec::new();
    let configurations_dir = config_dir.join("configurations");
    if let Ok(entries) = fs::read_dir(configurations_dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("config_"))
            {
                values.extend(load_gcloud_project_values(&entry.path()));
            }
        }
    }
    dedup_sorted(values)
}

fn load_gcloud_project_values(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                return None;
            }
            let (key, value) = trimmed.split_once('=')?;
            (key.trim() == "project" && !value.trim().is_empty()).then(|| value.trim().to_string())
        })
        .collect()
}

fn azure_config_dir(home: &Option<String>, explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| PathBuf::from(home).join(".azure")))
        .unwrap_or_else(|| PathBuf::from(".azure"))
}

fn load_az_subscriptions(profile_file: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(profile_file) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    if let Some(subscriptions) = value
        .get("subscriptions")
        .and_then(serde_json::Value::as_array)
    {
        for subscription in subscriptions {
            values.extend(subscription.get("id").and_then(serde_json::Value::as_str));
        }
    }
    dedup_sorted(values.into_iter().map(str::to_string).collect())
}

fn find_maven_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| ancestor.join("pom.xml").is_file())
        .map(Path::to_path_buf)
}

fn load_maven_profiles(pom_file: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(pom_file) else {
        return Vec::new();
    };
    dedup_sorted(
        xml_blocks(&contents, "profile")
            .into_iter()
            .flat_map(|block| xml_tag_values(block, "id"))
            .collect(),
    )
}

fn load_maven_modules(pom_file: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(pom_file) else {
        return Vec::new();
    };
    dedup_sorted(
        xml_blocks(&contents, "modules")
            .into_iter()
            .flat_map(|block| xml_tag_values(block, "module"))
            .collect(),
    )
}

fn xml_blocks<'a>(contents: &'a str, tag: &str) -> Vec<&'a str> {
    let mut blocks = Vec::new();
    let mut rest = contents;
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");
    while let Some(start) = rest.find(&open_prefix) {
        let after_start = &rest[start..];
        let Some(open_end) = after_start.find('>') else {
            break;
        };
        let after_open = &after_start[open_end + 1..];
        let Some(close_start) = after_open.find(&close) else {
            break;
        };
        blocks.push(&after_open[..close_start]);
        rest = &after_open[close_start + close.len()..];
    }
    blocks
}

fn xml_tag_values(contents: &str, tag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = contents;
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    while let Some(start) = rest.find(&open) {
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        let value = after_open[..end].trim();
        if !value.is_empty() && !value.contains('<') {
            values.push(value.to_string());
        }
        rest = &after_open[end + close.len()..];
    }
    values
}

fn selected_ansible_inventory_paths(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
    project_root: &Path,
) -> Vec<PathBuf> {
    let words = parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    for (index, token) in words.iter().enumerate() {
        if *token == "-i" || *token == "--inventory" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            if !value.is_empty() && !value.starts_with('-') {
                values.push(path_from_token(current_dir, value));
            }
            continue;
        }
        if let Some(value) = token
            .strip_prefix("--inventory=")
            .or_else(|| token.strip_prefix("-i="))
            && !value.is_empty()
        {
            values.push(path_from_token(current_dir, value));
        }
    }

    if values.is_empty() {
        values.extend([
            project_root.join("inventory"),
            project_root.join("hosts"),
            project_root.join("ansible").join("inventory"),
            current_dir.join("inventory"),
            current_dir.join("hosts"),
        ]);
    }
    dedup_sorted_paths(values)
}

fn path_from_token(current_dir: &Path, token: &str) -> PathBuf {
    let path = PathBuf::from(normalize_path_token(token));
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

fn load_ansible_inventory_values(paths: &[PathBuf]) -> Vec<String> {
    let mut values = Vec::new();
    for path in paths {
        values.extend(load_ansible_inventory_path(path));
    }
    dedup_sorted(values)
}

fn load_ansible_inventory_path(path: &Path) -> Vec<String> {
    if path.is_file() {
        return parse_ansible_inventory_file(path);
    }
    if !path.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_file() {
            values.extend(parse_ansible_inventory_file(&child));
        }
    }
    values
}

fn parse_ansible_inventory_file(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_ansible_inventory_values(&contents)
}

fn parse_ansible_inventory_values(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(section) = trimmed.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            let name = section
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '\'']);
            if is_ansible_inventory_name(name) {
                values.push(name.to_string());
            }
            continue;
        }
        if let Some(key) = trimmed.strip_suffix(':') {
            let key = key.trim().trim_matches(['"', '\'']);
            if is_ansible_inventory_name(key)
                && !matches!(key, "all" | "hosts" | "children" | "vars")
            {
                values.push(key.to_string());
            }
            continue;
        }
        let host = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(['"', '\'']);
        if is_ansible_inventory_name(host)
            && !host.contains('=')
            && !matches!(host, "all" | "hosts" | "children" | "vars")
        {
            values.push(host.to_string());
        }
    }
    dedup_sorted(values)
}

fn is_ansible_inventory_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
}

fn dedup_sorted_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

fn find_terraform_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| ancestor.join(".terraform").is_dir())
        .map(Path::to_path_buf)
}

fn load_terraform_workspaces(root: &Path) -> Vec<String> {
    let terraform_dir = root.join(".terraform");
    let mut values = vec!["default".to_string()];
    if let Ok(current) = fs::read_to_string(terraform_dir.join("environment")) {
        let current = current.trim();
        if !current.is_empty() {
            values.push(current.to_string());
        }
    }
    let state_dir = terraform_dir.join("terraform.tfstate.d");
    if let Ok(entries) = fs::read_dir(state_dir) {
        values.extend(
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string)),
        );
    }
    dedup_sorted(values)
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
    fn node_bin_root_walks_up_from_workspace_subdir() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let package_dir = dir.path().join("packages").join("web").join("src");
        fs::create_dir_all(&package_dir).unwrap();

        assert_eq!(
            find_node_bin_root(&package_dir).as_deref(),
            Some(dir.path())
        );
    }

    #[test]
    fn python_module_loader_reads_dependencies_and_project_modules() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"fast-api>=1\", \"google-cloud-storage\"]\n",
        )
        .unwrap();
        let package_dir = dir.path().join("src").join("demo_app");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(package_dir.join("__init__.py"), "").unwrap();
        fs::write(package_dir.join("cli.py"), "").unwrap();
        fs::write(dir.path().join("tool.py"), "").unwrap();

        assert_eq!(
            load_python_modules(dir.path()),
            vec![
                "demo_app".to_string(),
                "demo_app.cli".to_string(),
                "fast_api".to_string(),
                "google_cloud_storage".to_string(),
                "tool".to_string(),
            ]
        );
    }

    #[test]
    fn node_workspace_loader_reads_package_json_and_pnpm_workspace() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "workspaces": ["packages/*"] }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n  - '!ignored/*'\n",
        )
        .unwrap();
        let web_dir = dir.path().join("packages").join("web");
        let api_dir = dir.path().join("apps").join("api");
        fs::create_dir_all(&web_dir).unwrap();
        fs::create_dir_all(&api_dir).unwrap();
        fs::write(web_dir.join("package.json"), r#"{ "name": "@demo/web" }"#).unwrap();
        fs::write(api_dir.join("package.json"), r#"{ "name": "api" }"#).unwrap();

        assert_eq!(
            find_node_workspace_root(&web_dir).as_deref(),
            Some(dir.path())
        );
        assert_eq!(
            load_node_workspaces(dir.path()),
            vec![
                "@demo/web".to_string(),
                "api".to_string(),
                "apps/api".to_string(),
                "packages/web".to_string(),
            ]
        );
    }

    #[test]
    fn cloud_and_terraform_loaders_read_local_config_only() {
        let dir = tempdir().unwrap();
        let aws_dir = dir.path().join(".aws");
        fs::create_dir_all(&aws_dir).unwrap();
        fs::write(
            aws_dir.join("config"),
            "[default]\nregion = us-east-1\n[profile dev]\nregion = us-west-2\n",
        )
        .unwrap();
        fs::write(
            aws_dir.join("credentials"),
            "[prod]\naws_access_key_id = test\n",
        )
        .unwrap();
        assert_eq!(
            load_aws_profiles(&aws_dir.join("config"), &aws_dir.join("credentials")),
            vec!["default".to_string(), "dev".to_string(), "prod".to_string()]
        );

        let gcloud_dir = dir.path().join("gcloud");
        let configs_dir = gcloud_dir.join("configurations");
        fs::create_dir_all(&configs_dir).unwrap();
        fs::write(configs_dir.join("config_dev"), "project = demo-dev\n").unwrap();
        fs::write(configs_dir.join("config_prod"), "project = demo-prod\n").unwrap();
        assert_eq!(
            load_gcloud_configurations(&gcloud_dir),
            vec!["dev".to_string(), "prod".to_string()]
        );
        assert_eq!(
            load_gcloud_projects(&gcloud_dir),
            vec!["demo-dev".to_string(), "demo-prod".to_string()]
        );

        let azure_dir = dir.path().join(".azure");
        fs::create_dir_all(&azure_dir).unwrap();
        fs::write(
            azure_dir.join("azureProfile.json"),
            r#"{
                "subscriptions": [
                    { "id": "0000-1111", "name": "Dev Subscription" },
                    { "id": "2222-3333", "name": "Prod Subscription" }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(
            load_az_subscriptions(&azure_dir.join("azureProfile.json")),
            vec!["0000-1111".to_string(), "2222-3333".to_string()]
        );
        assert!(
            !load_az_subscriptions(&azure_dir.join("azureProfile.json"))
                .iter()
                .any(|value| value.contains(' ')),
            "subscription names are not shell-safe as raw argument candidates"
        );

        let terraform_dir = dir.path().join(".terraform");
        fs::create_dir_all(terraform_dir.join("terraform.tfstate.d").join("dev")).unwrap();
        fs::write(terraform_dir.join("environment"), "staging\n").unwrap();
        assert_eq!(
            load_terraform_workspaces(dir.path()),
            vec![
                "default".to_string(),
                "dev".to_string(),
                "staging".to_string(),
            ]
        );
    }

    #[test]
    fn maven_loaders_read_profiles_and_modules_from_pom() {
        let dir = tempdir().unwrap();
        let pom = dir.path().join("pom.xml");
        fs::write(
            &pom,
            r#"
<project>
  <modules>
    <module>service-api</module>
    <module>service-web</module>
  </modules>
  <profiles>
    <profile><id>dev</id></profile>
    <profile><id>release</id></profile>
  </profiles>
</project>
"#,
        )
        .unwrap();

        assert_eq!(
            load_maven_modules(&pom),
            vec!["service-api".to_string(), "service-web".to_string()]
        );
        assert_eq!(
            load_maven_profiles(&pom),
            vec!["dev".to_string(), "release".to_string()]
        );
    }

    #[test]
    fn ansible_inventory_parser_reads_ini_and_yaml_names() {
        let inventory = r#"
[web]
web-1 ansible_host=192.0.2.10

[db:children]
postgres

all:
  children:
    api:
      hosts:
        api-1:
"#;

        assert_eq!(
            parse_ansible_inventory_values(inventory),
            vec![
                "api".to_string(),
                "api-1".to_string(),
                "db".to_string(),
                "postgres".to_string(),
                "web".to_string(),
                "web-1".to_string(),
            ]
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
