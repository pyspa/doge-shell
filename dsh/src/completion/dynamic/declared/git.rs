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
        "git.alias" => {
            collector.collect_git_alias_candidates(current_dir, current_token, cached_only)
        }
        "git.config_key" => {
            collector.collect_git_config_key_candidates(current_dir, current_token, cached_only)
        }
        "git.branch" => {
            collector.collect_git_branch_candidates(current_dir, current_token, cached_only)
        }
        "git.checkout_target" => collector.collect_git_checkout_target_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "git.changed_path" => {
            collector.collect_git_changed_path_candidates(current_dir, current_token, cached_only)
        }
        "git.push_branch" => collector.collect_git_push_branch_candidates(
            current_dir,
            selected_git_remote(parsed_command_line),
            current_token,
            cached_only,
        ),
        "git.remote" => {
            collector.collect_git_remote_candidates(current_dir, current_token, cached_only)
        }
        "git.remote_branch" => collector.collect_git_remote_branch_candidates(
            current_dir,
            selected_git_remote(parsed_command_line),
            current_token,
            cached_only,
        ),
        "git.revision" => {
            collector.collect_git_revision_candidates(current_dir, current_token, cached_only)
        }
        "git.stash" => {
            collector.collect_git_stash_candidates(current_dir, current_token, cached_only)
        }
        "git.tag" => collector.collect_git_tag_candidates(current_dir, current_token, cached_only),
        "git.worktree" => {
            collector.collect_git_worktree_candidates(current_dir, current_token, cached_only)
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
