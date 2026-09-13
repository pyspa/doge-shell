//! The per-ecosystem candidate collectors `collect` dispatches to: cargo,
//! npm/pnpm workspaces, Python projects and their environment managers, Go, the
//! cloud CLIs, Maven, Ansible, Terraform and the remaining one-off tools.
use super::*;

impl DynamicCompletionProvider {
    pub(crate) fn collect_cargo_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
        else {
            return Vec::new();
        };

        let (kind, description) = match option_name.as_str() {
            "-p" | "--package" => (CargoMetadataValueKind::Package, "cargo package"),
            "--bin" => (CargoMetadataValueKind::Bin, "cargo binary target"),
            "--example" => (CargoMetadataValueKind::Example, "cargo example target"),
            _ => return Vec::new(),
        };

        self.collect_cargo_metadata_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            kind,
            description,
            cached_only,
        )
    }

    pub(crate) fn collect_python_project_dependency_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = self.cached_project_root(current_dir);
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
        let project_root = self.cached_project_root(current_dir);
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
            .unwrap_or_else(|| self.cached_project_root(current_dir));
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
        let project_root = self.cached_project_root(current_dir);
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
        let project_root = self.cached_project_root(current_dir);
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
        let maven_root =
            find_maven_root(current_dir).unwrap_or_else(|| self.cached_project_root(current_dir));
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
        let maven_root =
            find_maven_root(current_dir).unwrap_or_else(|| self.cached_project_root(current_dir));
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
        let project_root = self.cached_project_root(current_dir);
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
            .unwrap_or_else(|| self.cached_project_root(current_dir));
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

    pub(crate) fn collect_nox_session_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root =
            find_nox_root(current_dir).unwrap_or_else(|| self.cached_project_root(current_dir));
        let noxfile = project_root.join("noxfile.py");
        self.collect_cached_value_candidates(
            "nox",
            "session",
            project_root,
            current_token,
            "nox session",
            cached_only,
            move || Ok(load_nox_sessions(&noxfile)),
        )
    }

    pub(crate) fn collect_tox_environment_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root =
            find_tox_root(current_dir).unwrap_or_else(|| self.cached_project_root(current_dir));
        let scope = project_root.clone();
        self.collect_cached_value_candidates(
            "tox",
            "environment",
            scope,
            current_token,
            "tox environment",
            cached_only,
            move || Ok(load_tox_environments(&project_root)),
        )
    }

    pub(crate) fn collect_hatch_environment_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root =
            find_hatch_root(current_dir).unwrap_or_else(|| self.cached_project_root(current_dir));
        let scope = project_root.clone();
        self.collect_cached_value_candidates(
            "hatch",
            "environment",
            scope,
            current_token,
            "hatch environment",
            cached_only,
            move || Ok(load_hatch_environments(&project_root)),
        )
    }

    pub(crate) fn collect_pre_commit_hook_id_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = find_pre_commit_root(current_dir)
            .unwrap_or_else(|| self.cached_project_root(current_dir));
        let scope = project_root.clone();
        self.collect_cached_value_candidates(
            "pre-commit",
            "hook-id",
            scope,
            current_token,
            "pre-commit hook id",
            cached_only,
            move || Ok(load_pre_commit_hook_ids(&project_root)),
        )
    }

    pub(super) fn collect_bacon_job_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = find_ancestor_containing(current_dir, &["bacon.toml"])
            .unwrap_or_else(|| self.cached_project_root(current_dir));
        let scope = project_root.clone();
        self.collect_cached_value_candidates(
            "bacon",
            "job",
            scope,
            current_token,
            "bacon job",
            cached_only,
            move || {
                Ok(load_toml_table_keys(
                    &project_root.join("bacon.toml"),
                    &["jobs"],
                ))
            },
        )
    }

    pub(super) fn collect_pdm_script_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = find_ancestor_containing(current_dir, &["pyproject.toml"])
            .unwrap_or_else(|| self.cached_project_root(current_dir));
        let scope = project_root.clone();
        self.collect_cached_value_candidates(
            "pdm",
            "script",
            scope,
            current_token,
            "PDM script",
            cached_only,
            move || {
                Ok(load_toml_table_keys(
                    &project_root.join("pyproject.toml"),
                    &["tool", "pdm", "scripts"],
                ))
            },
        )
    }

    pub(super) fn collect_pipenv_script_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = find_ancestor_containing(current_dir, &["Pipfile"])
            .unwrap_or_else(|| self.cached_project_root(current_dir));
        let scope = project_root.clone();
        self.collect_cached_value_candidates(
            "pipenv",
            "script",
            scope,
            current_token,
            "Pipenv script",
            cached_only,
            move || {
                Ok(load_toml_table_keys(
                    &project_root.join("Pipfile"),
                    &["scripts"],
                ))
            },
        )
    }

    pub(super) fn collect_ghq_repository_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("ghq");
        let current_dir = current_dir.to_path_buf();
        let scope = self
            .env_var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| current_dir.clone());
        self.collect_cached_value_candidates(
            "ghq",
            "repository",
            scope,
            current_token,
            "ghq repository",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_plain_lines(&run_command_lines(
                    &command_path,
                    &["list"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(super) fn collect_jj_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        kind: &str,
        args: &'static [&'static str],
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = selected_jj_repository(parsed_command_line, current_dir)
            .or_else(|| find_jj_root(current_dir))
            .unwrap_or_else(|| self.cached_project_root(current_dir));
        let command_path = self.resolve_command_path("jj");
        let scope = project_root.clone();
        let current_dir = current_dir.to_path_buf();
        let description = format!("jj {kind}");
        self.collect_cached_value_candidates(
            "jj",
            kind,
            scope,
            current_token,
            &description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let repository = project_root.to_string_lossy().into_owned();
                let mut command_args = vec!["--repository", repository.as_str()];
                command_args.extend_from_slice(args);
                Ok(parse_plain_lines(&run_command_lines(
                    &command_path,
                    &command_args,
                    &current_dir,
                )?))
            },
        )
    }

    pub(super) fn collect_meson_target_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let project_root = find_ancestor_containing(current_dir, &["meson.build"])
            .unwrap_or_else(|| self.cached_project_root(current_dir));
        let build_dir = selected_meson_build_dir(parsed_command_line, &project_root);
        let command_path = self.resolve_command_path("meson");
        let scope = build_dir.clone();
        self.collect_cached_value_candidates(
            "meson",
            "target",
            scope,
            current_token,
            "Meson target",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let build_dir = build_dir.to_string_lossy().into_owned();
                let output = run_command_stdout(
                    &command_path,
                    &["introspect", "--targets", build_dir.as_str()],
                    &project_root,
                )?;
                Ok(parse_meson_targets(&output))
            },
        )
    }

    fn env_var(&self, key: &str) -> Option<String> {
        self.environment
            .read()
            .get_var(key)
            .or_else(|| std::env::var(key).ok())
    }
}
