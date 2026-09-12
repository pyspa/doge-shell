use super::{
    CachePolicy, CandidateType, CompletionContext, DynamicCompletionProvider, EnhancedCandidate,
    ParsedCommandLine, dedup_sorted,
};
use std::path::Path;

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::EnhancedCandidate>> {
    use super::*;

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
            selected_remote(parsed_command_line),
            current_token,
            cached_only,
        ),
        "git.remote" => {
            collector.collect_git_remote_candidates(current_dir, current_token, cached_only)
        }
        "git.remote_branch" => collector.collect_git_remote_branch_candidates(
            current_dir,
            selected_remote(parsed_command_line),
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

pub(super) fn selected_remote(parsed_command_line: &super::ParsedCommandLine) -> Option<&str> {
    parsed_command_line
        .specified_arguments
        .first()
        .map(String::as_str)
        .filter(|remote| !remote.is_empty())
        .filter(|remote| *remote != parsed_command_line.current_token)
}

impl DynamicCompletionProvider {
    /// Two cases the declarative JSON path (`completions/git.json`) cannot
    /// express, so this still handles them by hand:
    /// - the bare subcommand position (`git co<TAB>`), where a user-defined
    ///   git alias should complete alongside the built-in subcommand names -
    ///   `argument_type_for_completion_context` never resolves a `Dynamic`
    ///   provider for `CompletionContext::SubCommand`, by design (that
    ///   position lists subcommand *names*, not argument values);
    /// - `git restore -s/--source <TAB>`, an option value that names a
    ///   revision, keyed off a specific option rather than a plain argument
    ///   position.
    ///
    /// Every other position - every `CompletionContext::Argument` and the
    /// inferred-subcommand case - is declared directly in
    /// `completions/git.json` and reaches the same
    /// `collect_git_*_candidates` methods this file used to dispatch to by
    /// hand. `completion::integrated::tests::
    /// every_hand_dispatched_git_argument_case_matches_the_declared_json_provider`
    /// is what proved that removal safe; extend it before removing a case
    /// from here.
    pub(crate) fn collect_git_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let current_token = parsed_command_line.current_token.as_str();

        if parsed_command_line.subcommand_path.is_empty()
            && matches!(
                parsed_command_line.completion_context,
                CompletionContext::SubCommand
            )
        {
            return self
                .collect_git_alias_candidates(current_dir, current_token, cached_only)
                .into_iter()
                .map(|mut candidate| {
                    candidate.candidate_type = CandidateType::SubCommand;
                    candidate
                })
                .collect();
        }

        if let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
            && parsed_command_line
                .subcommand_path
                .first()
                .map(String::as_str)
                == Some("restore")
            && matches!(option_name.as_str(), "-s" | "--source")
        {
            return self.collect_git_revision_candidates(current_dir, current_token, cached_only);
        }

        Vec::new()
    }
}

pub(super) fn parse_remote_branches(lines: &[String], remote: Option<&str>) -> Vec<String> {
    let mut values = Vec::new();
    for line in lines {
        if line.ends_with("/HEAD") || line == "HEAD" {
            continue;
        }
        let Some((candidate_remote, branch)) = line.split_once('/') else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        if let Some(remote) = remote
            && !remote.is_empty()
            && candidate_remote != remote
        {
            continue;
        }
        values.push(branch.to_string());
    }
    dedup_sorted(values)
}

pub(super) fn parse_stash_refs(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split(':').next().map(str::to_string))
            .collect(),
    )
}

pub(super) fn parse_status_porcelain_paths(output: &str) -> Vec<String> {
    let records = output
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }
        let status = &record[..2];
        let path = record[3..].trim();
        if !path.is_empty() {
            values.push(path.to_string());
        }
        if status.contains('R') || status.contains('C') {
            index += 2;
        } else {
            index += 1;
        }
    }
    dedup_sorted(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_parsers_normalize_remote_stash_and_status_values() {
        assert_eq!(
            parse_remote_branches(
                &[
                    "origin/HEAD".to_string(),
                    "origin/main".to_string(),
                    "upstream/release".to_string(),
                ],
                Some("origin"),
            ),
            vec!["main"]
        );
        assert_eq!(
            parse_stash_refs(&["stash@{0}: WIP".to_string()]),
            vec!["stash@{0}"]
        );
        assert_eq!(
            parse_status_porcelain_paths(" M src/lib.rs\0R  old.rs\0new.rs\0"),
            vec!["old.rs", "src/lib.rs"]
        );
    }
}
