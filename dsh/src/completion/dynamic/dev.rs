//! Developer-toolchain dynamic completion providers: one collector method per
//! tool on `DynamicCompletionProvider`, backed by value parsers/root-finders
//! split out by ecosystem into the sibling modules below. Root/path helpers
//! used across more than one ecosystem (`find_ancestor_containing`,
//! `resolve_project_path`, `resolve_command_path_token`) and the generic
//! line parsers several `LOCAL_SPECS` rows share (`parse_plain_lines` and
//! friends) stay in this file rather than picking one ecosystem to own them.
use super::{
    CachePolicy, CargoMetadataValueKind, CompletionContext, DynamicCompletionProvider,
    completion_words, dedup_sorted, run_command_lines, run_command_stdout,
};
use crate::completion::integrated::EnhancedCandidate;
use crate::completion::parser::ParsedCommandLine;
use crate::completion::shell_path::normalize_path_token;
use std::fs;
use std::path::{Path, PathBuf};

mod ansible;
mod cloud;
mod go;
mod jvm;
mod misc;
mod node;
mod python;
mod rust;
mod terraform;

use ansible::*;
use cloud::*;
use go::*;
use jvm::*;
use misc::*;
use node::*;
use python::*;
use rust::*;
use terraform::*;

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

fn resolve_command_path_token(current_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(normalize_path_token(value));
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
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

#[cfg(test)]
mod tests;
