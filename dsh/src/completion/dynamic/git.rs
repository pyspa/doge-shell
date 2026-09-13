//! Git-specific dynamic completion: branches, remotes, worktrees, aliases,
//! config keys, tags, stashes, and changed paths. `collect` dispatches by
//! provider id (the JSON-declared path); `collect_git_candidates` is the one
//! hand-written case the declarative path can't express (a bare subcommand
//! position where user aliases should complete alongside built-ins).
use super::{
    CachePolicy, CandidateType, CompletionContext, DynamicCommandCacheKind,
    DynamicCompletionProvider, EnhancedCandidate, ParsedCommandLine, dedup_sorted,
    run_command_lines, run_command_stdout,
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
    /// One case the declarative JSON path (`completions/git.json`) cannot
    /// express, so this still handles it by hand: the bare subcommand
    /// position (`git co<TAB>`), where a user-defined git alias should
    /// complete alongside the built-in subcommand names.
    /// `argument_type_for_completion_context` never resolves a `Dynamic`
    /// provider for `CompletionContext::SubCommand`, by design (that
    /// position lists subcommand *names*, not argument values), so there is
    /// no JSON equivalent to fall back to here.
    ///
    /// Every other position - every `CompletionContext::Argument`, the
    /// inferred-subcommand case, and every option value (including
    /// `git restore -s/--source`, which `completions/git.json` already
    /// declares as `Dynamic { provider: "git.revision" }` on that option) -
    /// is declared directly in `completions/git.json` and reaches the same
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

        Vec::new()
    }

    fn collect_git_branch_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitBranch,
            scope_dir,
            current_token,
            "git branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(
                        &command_path,
                        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                        &current_dir,
                    )
                }
            },
        )
    }
    fn collect_git_remote_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitRemote,
            scope_dir,
            current_token,
            "git remote",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(&command_path, &["remote"], &current_dir)
                }
            },
        )
    }
    fn collect_git_worktree_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitWorktree,
            scope_dir,
            current_token,
            "git worktree",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    Ok(run_command_lines(
                        &command_path,
                        &["worktree", "list", "--porcelain"],
                        &current_dir,
                    )?
                    .into_iter()
                    .filter_map(|line| line.strip_prefix("worktree ").map(str::to_string))
                    .collect())
                }
            },
        )
    }
    fn collect_git_checkout_target_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "checkout-target",
            scope_dir,
            current_token,
            "git branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let mut values = run_command_lines(
                        &command_path,
                        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                        &current_dir,
                    )?;
                    values.extend(parse_remote_branches(
                        &run_command_lines(
                            &command_path,
                            &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
                            &current_dir,
                        )?,
                        None,
                    ));
                    Ok(dedup_sorted(values))
                }
            },
        )
    }
    /// User-defined git aliases (`git config --get-regexp ^alias.`), surfaced at
    /// the subcommand position (`git co<TAB>`).
    fn collect_git_alias_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "alias",
            scope_dir,
            current_token,
            "git alias",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let lines = run_command_lines(
                        &command_path,
                        &["config", "--get-regexp", "^alias\\."],
                        &current_dir,
                    )?;
                    let mut values = Vec::new();
                    for line in lines {
                        // e.g. "alias.co checkout" -> "co"
                        if let Some(rest) = line.strip_prefix("alias.")
                            && let Some(name) = rest.split_whitespace().next()
                        {
                            values.push(name.to_string());
                        }
                    }
                    Ok(dedup_sorted(values))
                }
            },
        )
    }
    /// Existing git config keys (`git config --name-only --list`), for
    /// `git config <key>` completion.
    fn collect_git_config_key_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "config-key",
            scope_dir,
            current_token,
            "git config key",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let lines = run_command_lines(
                        &command_path,
                        &["config", "--name-only", "--list"],
                        &current_dir,
                    )?;
                    Ok(dedup_sorted(lines))
                }
            },
        )
    }
    fn collect_git_remote_branch_candidates(
        &self,
        current_dir: &Path,
        remote: Option<&str>,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        let remote = remote.map(str::to_string);
        let value_kind = format!(
            "remote-branch:{}",
            remote
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("*")
        );
        self.collect_cached_value_candidates(
            "git",
            &value_kind,
            scope_dir,
            current_token,
            "git remote branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parse_remote_branches(
                        &run_command_lines(
                            &command_path,
                            &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
                            &current_dir,
                        )?,
                        remote.as_deref(),
                    ))
                }
            },
        )
    }
    fn collect_git_push_branch_candidates(
        &self,
        current_dir: &Path,
        remote: Option<&str>,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        let remote = remote.map(str::to_string);
        let value_kind = format!(
            "push-branch:{}",
            remote
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("*")
        );
        self.collect_cached_value_candidates(
            "git",
            &value_kind,
            scope_dir,
            current_token,
            "git branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let mut values = run_command_lines(
                        &command_path,
                        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                        &current_dir,
                    )?;
                    values.extend(parse_remote_branches(
                        &run_command_lines(
                            &command_path,
                            &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
                            &current_dir,
                        )?,
                        remote.as_deref(),
                    ));
                    Ok(dedup_sorted(values))
                }
            },
        )
    }
    fn collect_git_revision_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "revision",
            scope_dir,
            current_token,
            "git revision",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let mut values = run_command_lines(
                        &command_path,
                        &[
                            "for-each-ref",
                            "--format=%(refname:short)",
                            "refs/heads",
                            "refs/tags",
                        ],
                        &current_dir,
                    )?;
                    values.extend([
                        "HEAD".to_string(),
                        "FETCH_HEAD".to_string(),
                        "ORIG_HEAD".to_string(),
                    ]);
                    Ok(dedup_sorted(values))
                }
            },
        )
    }
    fn collect_git_tag_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "tag",
            scope_dir,
            current_token,
            "git tag",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    run_command_lines(&command_path, &["tag"], &current_dir)
                }
            },
        )
    }
    fn collect_git_stash_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "stash",
            scope_dir,
            current_token,
            "git stash",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parse_stash_refs(&run_command_lines(
                        &command_path,
                        &["stash", "list"],
                        &current_dir,
                    )?))
                }
            },
        )
    }
    fn collect_git_changed_path_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = self.cached_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "changed-path",
            scope_dir,
            current_token,
            "git changed path",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parse_status_porcelain_paths(&run_command_stdout(
                        &command_path,
                        &["status", "--porcelain", "-z"],
                        &current_dir,
                    )?))
                }
            },
        )
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
