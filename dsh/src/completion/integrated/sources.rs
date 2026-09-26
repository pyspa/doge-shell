//! The candidate sources that are not command completion: shell jobs, the
//! project and bookmark commands, directories, MCP entries, the package-manager
//! and task-runner scripts, and the external/fish fallbacks.
use super::*;
use crate::shell::job_selection::ActiveJobSelection;

impl IntegratedCompletionEngine {
    pub(super) fn collect_shell_job_candidates(
        &self,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let jobs = self.shell_jobs.read();
        let mut candidates = Vec::with_capacity(jobs.len() + 2);
        let selection = ActiveJobSelection::for_len(jobs.len());

        if let Some(index) = selection.current()
            && let Some((_, command, state)) = jobs.get(index)
        {
            candidates.push(EnhancedCandidate {
                text: "%+".to_string(),
                description: Some(format!("current job: {command} ({state})")),
                candidate_type: CandidateType::Argument,
                priority: 160,
            });
            candidates.push(EnhancedCandidate {
                text: "%%".to_string(),
                description: Some(format!("current job alias: {command} ({state})")),
                candidate_type: CandidateType::Argument,
                priority: 159,
            });
        }
        if let Some(index) = selection.previous()
            && let Some((_, command, state)) = jobs.get(index)
        {
            candidates.push(EnhancedCandidate {
                text: "%-".to_string(),
                description: Some(format!("previous job: {command} ({state})")),
                candidate_type: CandidateType::Argument,
                priority: 155,
            });
        }
        candidates.extend(
            jobs.iter()
                .map(|(job_id, command, state)| EnhancedCandidate {
                    text: format!("%{job_id}"),
                    description: Some(format!("{command} ({state})")),
                    candidate_type: CandidateType::Argument,
                    priority: 150,
                }),
        );
        candidates.retain(|candidate| matches_prefix(current_token, &candidate.text));
        candidates
    }

    pub(super) fn collect_pm_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        let current_token = parsed_command_line.current_token.as_str();
        match parsed_command_line.completion_context {
            CompletionContext::SubCommand => pm_subcommand_candidates(current_token),
            CompletionContext::Argument { arg_index, .. } => {
                let Some(subcommand) = parsed_command_line.subcommand_path.first() else {
                    return Vec::new();
                };
                match subcommand.as_str() {
                    "add" => match arg_index {
                        0 => self.collect_directory_candidates(current_token),
                        1 => self.collect_project_name_candidates_from_path(
                            parsed_command_line,
                            current_token,
                        ),
                        _ => Vec::new(),
                    },
                    "work" | "remove" | "rm" | "jump" => {
                        self.collect_project_candidates(current_token)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn collect_pj_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        self.collect_project_candidates(current_token)
    }

    /// Projects, most recently used first.
    ///
    /// The order has to travel in `priority`. `deduplicate_and_sort` re-sorts
    /// every candidate before the user sees it, and its last tiebreak is the
    /// candidate text, so a `Vec` that is merely *in* recency order comes back
    /// alphabetical and the sort above is dead code. `collect_shell_job_candidates`
    /// encodes `%+`/`%-`/`%N` the same way.
    ///
    /// Only the first `RECENCY_RANKS + 1` projects get a distinct priority: the
    /// steps have to stay inside this group's band, and 80 is where option
    /// candidates sit, which a project must never sort below. Everything past
    /// that ties at the floor and falls back to alphabetical.
    pub(super) fn collect_project_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        const BASE_PRIORITY: u32 = 90;
        const RECENCY_RANKS: u32 = 9;

        let Ok(mut projects) = project::list_projects() else {
            return Vec::new();
        };

        projects.sort_by_key(|project| std::cmp::Reverse(project.last_accessed));
        projects
            .into_iter()
            .filter(|project| matches_prefix(current_token, &project.name))
            // Rank what the user will actually see, so the filter cannot leave
            // a gap at the top of the list.
            .enumerate()
            .map(|(rank, project)| EnhancedCandidate {
                text: project.name,
                description: Some(project.path.display().to_string()),
                candidate_type: CandidateType::Argument,
                priority: BASE_PRIORITY - (rank as u32).min(RECENCY_RANKS),
            })
            .collect()
    }

    pub(super) fn collect_project_name_candidates_from_path(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let Some(path) = parsed_command_line.specified_arguments.first() else {
            return Vec::new();
        };

        let trimmed = path.trim_end_matches(&['/', '\\'][..]);
        let Some(name) = std::path::Path::new(trimmed)
            .file_name()
            .and_then(|s| s.to_str())
        else {
            return Vec::new();
        };

        if !matches_prefix(current_token, name) {
            return Vec::new();
        }

        vec![EnhancedCandidate {
            text: name.to_string(),
            description: Some("from path".to_string()),
            candidate_type: CandidateType::Argument,
            priority: 95,
        }]
    }

    pub(super) fn collect_directory_candidates(
        &self,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        match FileSystemGenerator::generate_directory_candidates(current_token) {
            Ok(candidates) => candidates
                .into_iter()
                .map(|candidate| self.convert_to_enhanced_candidate(candidate))
                .collect(),
            Err(e) => {
                warn!("Failed to load directory completions: {}", e);
                Vec::new()
            }
        }
    }

    pub(super) fn collect_mcp_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        let current_token = parsed_command_line.current_token.as_str();
        match parsed_command_line.completion_context {
            CompletionContext::SubCommand => mcp_subcommand_candidates(current_token),
            CompletionContext::Argument { .. } => {
                let Some(subcommand) = parsed_command_line.subcommand_path.first() else {
                    return Vec::new();
                };
                match subcommand.as_str() {
                    "connect" | "c" | "disconnect" | "d" => {
                        let env = self.environment.read();
                        let mut seen = std::collections::HashSet::new();
                        env.mcp_servers()
                            .iter()
                            .filter_map(|server| {
                                if !matches_prefix(current_token, &server.label) {
                                    return None;
                                }
                                if !seen.insert(server.label.clone()) {
                                    return None;
                                }
                                Some(EnhancedCandidate {
                                    text: server.label.clone(),
                                    description: mcp_description(server),
                                    candidate_type: CandidateType::Argument,
                                    priority: 90,
                                })
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn collect_package_run_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        if !leading_completion_words_match(parsed_command_line, &["run"]) {
            return Vec::new();
        }
        self.dynamic.collect_project_task_candidates(
            parsed_command_line,
            current_dir,
            JS_TASK_SOURCES,
        )
    }

    pub(super) fn collect_yarn_script_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        let leading_words = leading_completion_words(parsed_command_line);
        if !(leading_words.is_empty() || leading_words.as_slice() == ["run"]) {
            return Vec::new();
        }
        self.dynamic.collect_project_task_candidates(
            parsed_command_line,
            current_dir,
            JS_TASK_SOURCES,
        )
    }

    pub(super) fn collect_deno_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        if !leading_completion_words_match(parsed_command_line, &["task"]) {
            return Vec::new();
        }
        self.dynamic.collect_project_task_candidates(
            parsed_command_line,
            current_dir,
            DENO_TASK_SOURCES,
        )
    }

    pub(super) fn collect_top_level_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        sources: &[&str],
    ) -> Vec<EnhancedCandidate> {
        if !leading_completion_words(parsed_command_line).is_empty() {
            return Vec::new();
        }
        self.dynamic
            .collect_project_task_candidates(parsed_command_line, current_dir, sources)
    }

    pub(super) fn collect_external_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &ParsedCommandLine,
    ) -> CandidateBatch {
        let candidates = self.dynamic.collect_external_candidates(
            request.current_dir,
            request.input,
            request.cursor_pos,
            parsed_command_line,
        );
        if candidates.is_empty() {
            return CandidateBatch::empty();
        }

        CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
    }

    pub(super) fn collect_fish_fallback_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &ParsedCommandLine,
    ) -> CandidateBatch {
        if self.scalar_option_value_context(parsed_command_line) {
            return CandidateBatch::empty();
        }

        let candidates = self.dynamic.collect_fish_fallback_candidates(
            request.current_dir,
            request.input,
            request.cursor_pos,
            parsed_command_line,
        );
        if candidates.is_empty() {
            return CandidateBatch::empty();
        }

        CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
    }
}
