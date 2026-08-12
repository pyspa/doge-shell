pub(in super::super) fn collect(
    collector: &super::super::DynamicCompletionProvider,
    request: &super::super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::super::EnhancedCandidate>> {
    use super::super::*;

    let provider = request.provider.as_str();
    let scope = request.scope;
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "archive.entry" => collector.collect_archive_entry_candidates(
            parsed_command_line,
            current_dir,
            cached_only,
        ),
        "man.page" => collector.collect_man_page_candidates(current_token, cached_only),
        "project.task" => {
            if let Some(config) = project::completion_config(scope, parsed_command_line) {
                collector.collect_project_task_candidates_for_sources_with_mode(
                    parsed_command_line,
                    current_dir,
                    config.sources,
                    cached_only,
                    config.candidate_text,
                )
            } else {
                collector.collect_task_candidates(
                    parsed_command_line,
                    current_dir,
                    request.cache_policy,
                )
            }
        }
        "filesystem.type" => {
            collector.collect_filesystem_type_candidates(current_token, cached_only)
        }
        "ssh.host" => collector.collect_ssh_host_candidates(
            parsed_command_line,
            current_dir,
            parsed_command_line.command.as_str(),
            request.cache_policy,
        ),
        "shell.abbr" => collector.collect_shell_abbr_candidates(current_token),
        "shell.alias" => collector.collect_shell_alias_candidates(current_token),
        "shell.env_var" => collector.collect_shell_env_var_candidates(current_token),
        "shell.job" => Vec::new(),
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
