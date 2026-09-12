use super::{
    CachePolicy, CargoMetadataValueKind, CompletionContext, DynamicCompletionProvider,
    completion_words, dedup_sorted, run_command_lines, run_command_stdout,
};
use crate::completion::integrated::EnhancedCandidate;
use crate::completion::parser::ParsedCommandLine;
use crate::completion::shell_path::normalize_path_token;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// This family's rows for `local::collect` - see `local` for what belongs
/// here. Table only; routing is unaffected by which family's table a
/// provider's row lives in.
pub(super) const LOCAL_SPECS: &[super::local::LocalSpec] = &[
    super::local::LocalSpec {
        provider: "rustup.component",
        command_name: "rustup",
        value_kind: "component",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "rustup",
            args: &["component", "list"],
            parser: parse_rustup_components,
        },
        description: "rustup component",
    },
    super::local::LocalSpec {
        provider: "rustup.target",
        command_name: "rustup",
        value_kind: "target",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "rustup",
            args: &["target", "list"],
            parser: parse_rustup_targets,
        },
        description: "rustup target",
    },
    super::local::LocalSpec {
        provider: "cargo.installed_binary",
        command_name: "cargo",
        value_kind: "installed-binary",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "cargo",
            args: &["install", "--list"],
            parser: parse_cargo_installed_crates,
        },
        description: "cargo installed crate",
    },
    super::local::LocalSpec {
        provider: "bat.theme",
        command_name: "bat",
        value_kind: "theme",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "bat",
            args: &["--list-themes"],
            parser: parse_plain_lines,
        },
        description: "bat theme",
    },
    super::local::LocalSpec {
        provider: "bat.language",
        command_name: "bat",
        value_kind: "language",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "bat",
            args: &["--list-languages"],
            parser: parse_colon_prefixed_names,
        },
        description: "bat language",
    },
    super::local::LocalSpec {
        provider: "rg.file_type",
        command_name: "rg",
        value_kind: "file-type",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "rg",
            args: &["--type-list"],
            parser: parse_colon_prefixed_names,
        },
        description: "ripgrep file type",
    },
    super::local::LocalSpec {
        provider: "ffmpeg.encoder",
        command_name: "ffmpeg",
        value_kind: "encoder",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "ffmpeg",
            args: &["-hide_banner", "-encoders"],
            parser: parse_ffmpeg_table,
        },
        description: "ffmpeg encoder",
    },
    super::local::LocalSpec {
        provider: "ffmpeg.decoder",
        command_name: "ffmpeg",
        value_kind: "decoder",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "ffmpeg",
            args: &["-hide_banner", "-decoders"],
            parser: parse_ffmpeg_table,
        },
        description: "ffmpeg decoder",
    },
    super::local::LocalSpec {
        provider: "ffmpeg.format",
        command_name: "ffmpeg",
        value_kind: "format",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "ffmpeg",
            args: &["-hide_banner", "-formats"],
            parser: parse_ffmpeg_table,
        },
        description: "ffmpeg format",
    },
    super::local::LocalSpec {
        provider: "go.env_key",
        command_name: "go",
        value_kind: "env-key",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "go",
            args: &["env"],
            parser: parse_go_env_keys,
        },
        description: "go environment key",
    },
    super::local::LocalSpec {
        provider: "pipx.installed_package",
        command_name: "pipx",
        value_kind: "installed-package",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "pipx",
            args: &["list", "--short"],
            parser: parse_first_field_lines,
        },
        description: "pipx installed package",
    },
    super::local::LocalSpec {
        provider: "asdf.plugin",
        command_name: "asdf",
        value_kind: "plugin",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "asdf",
            args: &["plugin", "list"],
            parser: parse_first_field_lines,
        },
        description: "asdf plugin",
    },
    super::local::LocalSpec {
        provider: "mise.tool",
        command_name: "mise",
        value_kind: "tool",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "mise",
            args: &["ls", "--installed"],
            parser: parse_mise_tools,
        },
        description: "mise tool",
    },
    super::local::LocalSpec {
        provider: "op.item",
        command_name: "op",
        value_kind: "item",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "op",
            args: &["item", "list", "--format", "json"],
            parser: parse_op_items,
        },
        description: "1Password item",
    },
    super::local::LocalSpec {
        provider: "vagrant.box",
        command_name: "vagrant",
        value_kind: "box",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "vagrant",
            args: &["box", "list"],
            parser: parse_first_field_lines,
        },
        description: "vagrant box",
    },
    super::local::LocalSpec {
        provider: "code.extension",
        command_name: "code",
        value_kind: "extension",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "code",
            args: &["--list-extensions"],
            parser: parse_plain_lines,
        },
        description: "VS Code extension",
    },
    super::local::LocalSpec {
        provider: "golangci_lint.linter",
        command_name: "golangci-lint",
        value_kind: "linter",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "golangci-lint",
            args: &["linters"],
            parser: parse_golangci_linters,
        },
        description: "golangci-lint linter",
    },
];

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<EnhancedCandidate>> {
    use super::*;

    let provider = request.provider.as_str();
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "cargo.feature" => {
            collector.collect_cargo_feature_candidates(current_dir, current_token, cached_only)
        }
        "cargo.package" => collector.collect_cargo_metadata_candidates(
            current_dir,
            current_token,
            CargoMetadataValueKind::Package,
            "cargo package",
            cached_only,
        ),
        "cargo.bin" => collector.collect_cargo_metadata_candidates(
            current_dir,
            current_token,
            CargoMetadataValueKind::Bin,
            "cargo binary target",
            cached_only,
        ),
        "cargo.example" => collector.collect_cargo_metadata_candidates(
            current_dir,
            current_token,
            CargoMetadataValueKind::Example,
            "cargo example target",
            cached_only,
        ),
        "js.dependency" => collector.collect_js_dependency_candidates(
            parsed_command_line,
            current_dir,
            parsed_command_line.command.as_str(),
            cached_only,
        ),
        "pip.installed_package" => collector.collect_pip_installed_package_candidates(
            current_dir,
            parsed_command_line.command.as_str(),
            current_token,
            cached_only,
        ),
        "python.project_dependency" => collector.collect_python_project_dependency_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "python.module" => {
            collector.collect_python_module_candidates(current_dir, current_token, cached_only)
        }
        "node.bin" => {
            collector.collect_node_bin_candidates(current_dir, current_token, cached_only)
        }
        "node.workspace" => {
            collector.collect_node_workspace_candidates(current_dir, current_token, cached_only)
        }
        "go.package" => {
            collector.collect_go_package_candidates(current_dir, current_token, cached_only)
        }
        "maven.module" => {
            collector.collect_maven_module_candidates(current_dir, current_token, cached_only)
        }
        "maven.profile" => {
            collector.collect_maven_profile_candidates(current_dir, current_token, cached_only)
        }
        "terraform.workspace" => collector.collect_terraform_workspace_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "cargo.test" => collector.collect_cargo_metadata_candidates(
            current_dir,
            current_token,
            CargoMetadataValueKind::Test,
            "cargo test target",
            cached_only,
        ),
        "cargo.bench" => collector.collect_cargo_metadata_candidates(
            current_dir,
            current_token,
            CargoMetadataValueKind::Bench,
            "cargo bench target",
            cached_only,
        ),
        "nox.session" => {
            collector.collect_nox_session_candidates(current_dir, current_token, cached_only)
        }
        "tox.environment" => {
            collector.collect_tox_environment_candidates(current_dir, current_token, cached_only)
        }
        "hatch.environment" => {
            collector.collect_hatch_environment_candidates(current_dir, current_token, cached_only)
        }
        "pre_commit.hook_id" => {
            collector.collect_pre_commit_hook_id_candidates(current_dir, current_token, cached_only)
        }
        "bacon.job" => {
            collector.collect_bacon_job_candidates(current_dir, current_token, cached_only)
        }
        "pdm.script" => {
            collector.collect_pdm_script_candidates(current_dir, current_token, cached_only)
        }
        "pipenv.script" => {
            collector.collect_pipenv_script_candidates(current_dir, current_token, cached_only)
        }
        "ghq.repository" => {
            collector.collect_ghq_repository_candidates(current_dir, current_token, cached_only)
        }
        "jj.bookmark" => collector.collect_jj_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            "bookmark",
            &["bookmark", "list", "-T", r#"name ++ "\n""#],
            cached_only,
        ),
        "jj.revision" => collector.collect_jj_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            "revision",
            &[
                "log",
                "-r",
                "all()",
                "--no-graph",
                "--limit",
                "200",
                "-T",
                r#"change_id.short() ++ "\n""#,
            ],
            cached_only,
        ),
        "jj.workspace" => collector.collect_jj_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            "workspace",
            &["workspace", "list", "-T", r#"name ++ "\n""#],
            cached_only,
        ),
        "meson.target" => collector.collect_meson_target_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            cached_only,
        ),
        _ => {
            return platform::collect(
                collector,
                provider,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }
    })
}

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

    fn collect_bacon_job_candidates(
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

    fn collect_pdm_script_candidates(
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

    fn collect_pipenv_script_candidates(
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

    fn collect_ghq_repository_candidates(
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

    fn collect_jj_candidates(
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

    fn collect_meson_target_candidates(
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

fn parse_plain_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn parse_first_field_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
    )
}

/// Parses `op item list --format json` (a JSON array of item objects).
fn parse_op_items(lines: &[String]) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&lines.join("\n")) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .iter()
            .filter_map(|entry| entry.get("title").and_then(serde_json::Value::as_str))
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Parses `name: description` style listings such as `bat --list-languages`
/// (`Rust:rs`) and `rg --type-list` (`rust: *.rs`).
fn parse_colon_prefixed_names(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

/// `rustup component list` prints every component suffixed with the host
/// triple (`clippy-x86_64-unknown-linux-gnu`), but `rustup component add`
/// accepts the bare name. Strip the suffix shared by every entry.
fn parse_rustup_components(lines: &[String]) -> Vec<String> {
    let names = rustup_listing_names(lines);
    let Some(host) = shared_target_triple(&names) else {
        return dedup_sorted(names);
    };
    let suffix = format!("-{host}");
    dedup_sorted(
        names
            .iter()
            .map(|name| {
                name.strip_suffix(&suffix)
                    .filter(|stripped| !stripped.is_empty())
                    .unwrap_or(name)
                    .to_string()
            })
            .collect(),
    )
}

fn parse_rustup_targets(lines: &[String]) -> Vec<String> {
    dedup_sorted(rustup_listing_names(lines))
}

fn rustup_listing_names(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty() && !name.starts_with('('))
        .map(str::to_string)
        .collect()
}

/// Number of dash separated segments a target triple can span
/// (`aarch64-apple-darwin` through `armv7-unknown-linux-gnueabihf`).
const TARGET_TRIPLE_MIN_SEGMENTS: usize = 3;
const TARGET_TRIPLE_MAX_SEGMENTS: usize = 4;
/// How many components must end with a suffix before it is treated as the host
/// triple. Every non-host target contributes exactly one `rust-std-<triple>`
/// entry, so a threshold of two rules them out.
const HOST_TRIPLE_MIN_OCCURRENCES: usize = 2;

/// Returns the host target triple that `rustup component list` appends to
/// component names. The listing also carries one `rust-std-<triple>` row per
/// supported target, so no suffix is common to *every* name; the host triple is
/// instead the one shared by the locally installable components (`cargo`,
/// `clippy`, `rust-src`, ...).
///
/// Longer suffixes are preferred because a three segment suffix of a four
/// segment triple (`unknown-linux-gnu` inside `x86_64-unknown-linux-gnu`) is
/// necessarily more frequent while being the wrong answer.
fn shared_target_triple(names: &[String]) -> Option<String> {
    let segmented = names
        .iter()
        .map(|name| name.split('-').collect::<Vec<_>>())
        .collect::<Vec<_>>();

    for length in (TARGET_TRIPLE_MIN_SEGMENTS..=TARGET_TRIPLE_MAX_SEGMENTS).rev() {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for parts in &segmented {
            // Require a segment to survive stripping, so `cargo-<triple>` counts
            // but a bare triple does not.
            if parts.len() <= length {
                continue;
            }
            *counts
                .entry(parts[parts.len() - length..].join("-"))
                .or_default() += 1;
        }
        if let Some((suffix, _)) = counts
            .into_iter()
            .filter(|(_, count)| *count >= HOST_TRIPLE_MIN_OCCURRENCES)
            .max_by_key(|(_, count)| *count)
        {
            return Some(suffix);
        }
    }
    None
}

/// Parses `cargo install --list`, whose crate headers carry the version and end
/// with a colon (`ripgrep v14.1.0:`) while the binaries they installed follow on
/// their own lines. Indentation cannot be used to tell them apart because the
/// command runner trims every line before the parser sees it.
fn parse_cargo_installed_crates(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter(|line| line.trim_end().ends_with(':'))
            .filter_map(|line| line.split_whitespace().next())
            .map(|name| name.trim_end_matches(':'))
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Parses the tabular listings printed by `ffmpeg -encoders`, `-decoders` and
/// `-formats`. Every table starts after a row of dashes and then prints
/// `<flags> <name> <description>`, where multi-name formats are comma joined.
fn parse_ffmpeg_table(lines: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut in_table = false;
    for line in lines {
        let trimmed = line.trim();
        if !in_table {
            if !trimmed.is_empty() && trimmed.chars().all(|c| c == '-') {
                in_table = true;
            }
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        let Some(_flags) = fields.next() else {
            continue;
        };
        let Some(names) = fields.next() else {
            continue;
        };
        values.extend(
            names
                .split(',')
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        );
    }
    dedup_sorted(values)
}

/// Parses `go env`, which prints `KEY='value'` (or `set KEY=value` on Windows).
/// Keys are upper case but may carry digits (`GO111MODULE`, `GOAMD64`, `GO386`).
fn parse_go_env_keys(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.trim().split_once('='))
            .map(|(key, _)| key.trim().trim_start_matches("set ").to_string())
            .filter(|key| {
                key.starts_with(|c: char| c.is_ascii_uppercase())
                    && key
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
            .collect(),
    )
}

/// Parses `mise ls --installed`, dropping the `Tool Version ...` header row.
fn parse_mise_tools(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| !name.is_empty() && *name != "Tool")
            .map(str::to_string)
            .collect(),
    )
}

fn load_nox_sessions(noxfile: &Path) -> Vec<String> {
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
fn parse_nox_sessions(contents: &str) -> Vec<String> {
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
fn decorator_name_argument(line: &str) -> Option<String> {
    let rest = line.split_once("name=")?.1.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let value = rest[quote.len_utf8()..].split(quote).next()?;
    (!value.is_empty()).then(|| value.to_string())
}

fn load_tox_environments(project_root: &Path) -> Vec<String> {
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
fn parse_tox_ini_environments(contents: &str) -> Vec<String> {
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

fn split_tox_envlist(value: &str) -> Vec<String> {
    value
        .split_once('#')
        .map_or(value, |(before_comment, _)| before_comment)
        .split([',', ' '])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn load_hatch_environments(project_root: &Path) -> Vec<String> {
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

fn load_toml_table_keys(path: &Path, table_path: &[&str]) -> Vec<String> {
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

fn load_pre_commit_hook_ids(project_root: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(project_root.join(".pre-commit-config.yaml")) else {
        return Vec::new();
    };
    parse_pre_commit_hook_ids(&contents)
}

/// Extracts `id:` values from `.pre-commit-config.yaml`. The file is scanned
/// line by line rather than parsed as YAML so that no extra dependency is
/// needed and partially written configs still yield candidates.
fn parse_pre_commit_hook_ids(contents: &str) -> Vec<String> {
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

fn find_nox_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &["noxfile.py"])
}

fn find_tox_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &["tox.ini", "pyproject.toml"])
}

fn find_hatch_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &["hatch.toml", "pyproject.toml"])
}

fn find_pre_commit_root(current_dir: &Path) -> Option<PathBuf> {
    find_ancestor_containing(current_dir, &[".pre-commit-config.yaml"])
}

fn find_jj_root(current_dir: &Path) -> Option<PathBuf> {
    current_dir
        .ancestors()
        .find(|candidate| candidate.join(".jj").exists())
        .map(Path::to_path_buf)
}

fn selected_jj_repository(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if matches!(*word, "-R" | "--repository")
            && let Some(value) = words
                .get(index + 1)
                .copied()
                .filter(|value| !value.is_empty())
        {
            return Some(resolve_command_path_token(current_dir, value));
        }
        if let Some(value) = word
            .strip_prefix("--repository=")
            .or_else(|| word.strip_prefix("-R="))
            .filter(|value| !value.is_empty())
        {
            return Some(resolve_command_path_token(current_dir, value));
        }
        if let Some(value) = word.strip_prefix("-R").filter(|value| !value.is_empty()) {
            return Some(resolve_command_path_token(current_dir, value));
        }
    }
    None
}

fn resolve_command_path_token(current_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(normalize_path_token(value));
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

fn selected_meson_build_dir(
    parsed_command_line: &ParsedCommandLine,
    project_root: &Path,
) -> PathBuf {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if matches!(*word, "-C" | "--builddir")
            && let Some(value) = words
                .get(index + 1)
                .copied()
                .filter(|value| !value.is_empty())
        {
            return resolve_project_path(project_root, value);
        }
        if let Some(value) = word
            .strip_prefix("--builddir=")
            .filter(|value| !value.is_empty())
        {
            return resolve_project_path(project_root, value);
        }
    }

    ["build", "builddir", "_build"]
        .into_iter()
        .map(|name| project_root.join(name))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| project_root.join("build"))
}

fn resolve_project_path(project_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn find_ancestor_containing(current_dir: &Path, markers: &[&str]) -> Option<PathBuf> {
    let mut dir = Some(current_dir);
    while let Some(candidate) = dir {
        if markers
            .iter()
            .any(|marker| candidate.join(marker).is_file())
        {
            return Some(candidate.to_path_buf());
        }
        dir = candidate.parent();
    }
    None
}

fn parse_golangci_linters(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let line = line.trim().trim_start_matches(['-', '*', ' ']);
                let (name, _) = line.split_once(':')?;
                let name = name.trim();
                (!name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
                .then(|| name.to_string())
            })
            .collect(),
    )
}

fn parse_meson_targets(output: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let Some(targets) = value.as_array() else {
        return Vec::new();
    };
    dedup_sorted(
        targets
            .iter()
            .filter_map(|target| target.get("name").and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .collect(),
    )
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
mod tests;
