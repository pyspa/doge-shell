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
    pub(crate) fn collect_git_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let current_token = parsed_command_line.current_token.as_str();
        let Some(primary_subcommand) = parsed_command_line.subcommand_path.first() else {
            // At the subcommand position (`git co<TAB>`), offer user-defined
            // git aliases alongside the built-in subcommands (from JSON).
            if matches!(
                parsed_command_line.completion_context,
                CompletionContext::SubCommand
            ) {
                return self
                    .collect_git_alias_candidates(current_dir, current_token, cached_only)
                    .into_iter()
                    .map(|mut candidate| {
                        candidate.candidate_type = CandidateType::SubCommand;
                        candidate
                    })
                    .collect();
            }
            return Vec::new();
        };
        let inferred_subcommand_arg_index =
            parsed_command_line.subcommand_path.len().saturating_sub(2);

        match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => {
                if primary_subcommand == "restore"
                    && matches!(option_name.as_str(), "-s" | "--source")
                {
                    self.collect_git_revision_candidates(current_dir, current_token, cached_only)
                } else {
                    Vec::new()
                }
            }
            CompletionContext::Argument { arg_index, .. } => self.collect_git_argument_candidates(
                primary_subcommand,
                *arg_index,
                parsed_command_line,
                current_dir,
                current_token,
                cached_only,
            ),
            CompletionContext::SubCommand => self.collect_git_argument_candidates(
                primary_subcommand,
                inferred_subcommand_arg_index,
                parsed_command_line,
                current_dir,
                current_token,
                cached_only,
            ),
            _ => Vec::new(),
        }
    }

    fn collect_git_argument_candidates(
        &self,
        primary_subcommand: &str,
        arg_index: usize,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        match primary_subcommand {
            "checkout" => {
                self.collect_git_checkout_target_candidates(current_dir, current_token, cached_only)
            }
            "switch" | "merge" | "rebase" => {
                self.collect_git_branch_candidates(current_dir, current_token, cached_only)
            }
            "add" | "restore" => {
                self.collect_git_changed_path_candidates(current_dir, current_token, cached_only)
            }
            "push" => {
                if arg_index == 0 {
                    self.collect_git_remote_candidates(current_dir, current_token, cached_only)
                } else {
                    self.collect_git_push_branch_candidates(
                        current_dir,
                        selected_remote(parsed_command_line),
                        current_token,
                        cached_only,
                    )
                }
            }
            "pull" | "fetch" => {
                if arg_index == 0 {
                    self.collect_git_remote_candidates(current_dir, current_token, cached_only)
                } else {
                    self.collect_git_remote_branch_candidates(
                        current_dir,
                        selected_remote(parsed_command_line),
                        current_token,
                        cached_only,
                    )
                }
            }
            "log" | "diff" | "show" | "reset" => {
                self.collect_git_revision_candidates(current_dir, current_token, cached_only)
            }
            "branch" => self.collect_git_branch_candidates(current_dir, current_token, cached_only),
            "tag" => self.collect_git_tag_candidates(current_dir, current_token, cached_only),
            "stash" => {
                let secondary = parsed_command_line
                    .subcommand_path
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("");
                if matches!(secondary, "pop" | "apply" | "drop") {
                    self.collect_git_stash_candidates(current_dir, current_token, cached_only)
                } else {
                    Vec::new()
                }
            }
            "remote" => {
                let secondary = parsed_command_line
                    .subcommand_path
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("");
                match secondary {
                    "remove" | "rename" | "show" | "get-url" | "set-url" => {
                        self.collect_git_remote_candidates(current_dir, current_token, cached_only)
                    }
                    _ => Vec::new(),
                }
            }
            "worktree" => {
                let secondary = parsed_command_line
                    .subcommand_path
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("");
                match secondary {
                    "remove" | "move" | "lock" | "unlock" | "repair" => self
                        .collect_git_worktree_candidates(current_dir, current_token, cached_only),
                    "add" if arg_index > 0 => {
                        self.collect_git_branch_candidates(current_dir, current_token, cached_only)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
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
