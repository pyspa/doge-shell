pub(in super::super) fn collect(
    collector: &super::super::DynamicCompletionProvider,
    request: &super::super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::super::EnhancedCandidate>> {
    use super::super::*;

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
        "rustup.toolchain" => {
            collector.collect_rustup_toolchain_candidates(current_dir, current_token, cached_only)
        }
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
        "rustup.component" => {
            collector.collect_rustup_component_candidates(current_token, cached_only)
        }
        "rustup.target" => collector.collect_rustup_target_candidates(current_token, cached_only),
        "cargo.installed_binary" => {
            collector.collect_cargo_installed_binary_candidates(current_token, cached_only)
        }
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
        "bat.theme" => collector.collect_bat_theme_candidates(current_token, cached_only),
        "bat.language" => collector.collect_bat_language_candidates(current_token, cached_only),
        "rg.file_type" => collector.collect_rg_file_type_candidates(current_token, cached_only),
        "ffmpeg.encoder" => collector.collect_ffmpeg_table_candidates(
            "encoder",
            current_token,
            "ffmpeg encoder",
            &["-hide_banner", "-encoders"],
            cached_only,
        ),
        "ffmpeg.decoder" => collector.collect_ffmpeg_table_candidates(
            "decoder",
            current_token,
            "ffmpeg decoder",
            &["-hide_banner", "-decoders"],
            cached_only,
        ),
        "ffmpeg.format" => collector.collect_ffmpeg_table_candidates(
            "format",
            current_token,
            "ffmpeg format",
            &["-hide_banner", "-formats"],
            cached_only,
        ),
        "go.env_key" => collector.collect_go_env_key_candidates(current_token, cached_only),
        "pipx.installed_package" => {
            collector.collect_pipx_installed_package_candidates(current_token, cached_only)
        }
        "asdf.plugin" => collector.collect_asdf_plugin_candidates(current_token, cached_only),
        "mise.tool" => collector.collect_mise_tool_candidates(current_token, cached_only),
        "code.extension" => collector.collect_code_extension_candidates(current_token, cached_only),
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
