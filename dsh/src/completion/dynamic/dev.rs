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
mod collectors;
mod parsers;
mod specs;
use parsers::*;
pub(super) use specs::LOCAL_SPECS;

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

#[cfg(test)]
mod tests;
