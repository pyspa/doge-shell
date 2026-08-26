use super::{
    CandidateType, DENO_PROJECT_TASK_SOURCES, DynamicCompletionProvider, EnhancedCandidate,
    GRADLE_PROJECT_TASK_SOURCES, JS_PROJECT_TASK_SOURCES, JUST_PROJECT_TASK_SOURCES,
    MAKE_PROJECT_TASK_SOURCES, MISE_PROJECT_TASK_SOURCES, NX_PROJECT_TASK_SOURCES,
    ProjectTaskCandidateText, ProjectTaskCompletionConfig, TASKFILE_PROJECT_TASK_SOURCES,
    TURBO_PROJECT_TASK_SOURCES, dedup_sorted, format_task_description, matches_prefix,
};
use crate::completion::parser::{CompletionContext, ParsedCommandLine};
use dsh_builtin::task;
use std::fs;
use std::path::Path;
use tracing::warn;

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::EnhancedCandidate>> {
    use super::*;

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
            if let Some(config) = completion_config(scope, parsed_command_line) {
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
        "direnv.rc" => {
            collector.collect_direnv_rc_candidates(current_dir, current_token, cached_only)
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

impl DynamicCompletionProvider {
    pub(crate) fn collect_direnv_rc_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "direnv",
            "rc",
            scope.clone(),
            current_token,
            ".envrc file",
            cached_only,
            move || Ok(load_direnv_rc_values(&scope)),
        )
    }

    pub(crate) fn collect_project_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        sources: &[&str],
    ) -> Vec<EnhancedCandidate> {
        self.collect_project_task_candidates_for_sources_with_mode(
            parsed_command_line,
            current_dir,
            sources,
            false,
            ProjectTaskCandidateText::Name,
        )
    }

    pub(super) fn collect_project_task_candidates_for_sources_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        sources: &[&str],
        cached_only: bool,
        candidate_text: ProjectTaskCandidateText,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match parsed_command_line.completion_context {
            CompletionContext::Command
            | CompletionContext::SubCommand
            | CompletionContext::Argument { .. } => {}
            _ => return Vec::new(),
        }

        let tasks = if cached_only {
            self.lookup_project_tasks_for_sources(current_dir, sources)
        } else {
            match self.load_project_tasks_for_sources(current_dir, sources) {
                Ok(tasks) => tasks,
                Err(err) => {
                    warn!("Failed to load project task completions: {}", err);
                    return Vec::new();
                }
            }
        };

        tasks
            .into_iter()
            .filter(|task| sources.contains(&task.source.as_str()))
            .filter_map(|task| {
                let text = candidate_text_for_task(&task, candidate_text);
                matches_prefix(current_token, &text).then_some((task, text))
            })
            .map(|(task, text)| EnhancedCandidate {
                text,
                description: Some(format_task_description(&task.source, &task.command)),
                candidate_type: CandidateType::Argument,
                priority: 125,
            })
            .collect()
    }
}

pub(super) fn completion_config(
    scope: Option<&str>,
    parsed_command_line: &ParsedCommandLine,
) -> Option<ProjectTaskCompletionConfig> {
    if let Some(scope_sources) = scope.and_then(sources_for_scope) {
        return Some(ProjectTaskCompletionConfig {
            sources: scope_sources,
            candidate_text: candidate_text_for_scope(scope),
        });
    }

    let sources: &'static [&'static str] = match parsed_command_line.command.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => Some(JS_PROJECT_TASK_SOURCES),
        "deno" => Some(DENO_PROJECT_TASK_SOURCES),
        "turbo" => Some(TURBO_PROJECT_TASK_SOURCES),
        "nx" => Some(NX_PROJECT_TASK_SOURCES),
        "mise" => Some(MISE_PROJECT_TASK_SOURCES),
        "task" => Some(TASKFILE_PROJECT_TASK_SOURCES),
        "just" => Some(JUST_PROJECT_TASK_SOURCES),
        "make" => Some(MAKE_PROJECT_TASK_SOURCES),
        "gradle" | "gradlew" => Some(GRADLE_PROJECT_TASK_SOURCES),
        _ => None,
    }?;
    Some(ProjectTaskCompletionConfig {
        sources,
        candidate_text: ProjectTaskCandidateText::Name,
    })
}

fn sources_for_scope(scope: &str) -> Option<&'static [&'static str]> {
    match scope {
        "js" | "package-json" | "npm" | "pnpm" | "yarn" | "bun" => Some(JS_PROJECT_TASK_SOURCES),
        "deno" => Some(DENO_PROJECT_TASK_SOURCES),
        "turbo" => Some(TURBO_PROJECT_TASK_SOURCES),
        "nx" | "nx.run" => Some(NX_PROJECT_TASK_SOURCES),
        "mise" => Some(MISE_PROJECT_TASK_SOURCES),
        "taskfile" | "task" => Some(TASKFILE_PROJECT_TASK_SOURCES),
        "just" => Some(JUST_PROJECT_TASK_SOURCES),
        "make" => Some(MAKE_PROJECT_TASK_SOURCES),
        "gradle" | "gradlew" => Some(GRADLE_PROJECT_TASK_SOURCES),
        _ => None,
    }
}

fn candidate_text_for_scope(scope: Option<&str>) -> ProjectTaskCandidateText {
    match scope {
        Some("nx.run") => ProjectTaskCandidateText::NxRunArgument,
        _ => ProjectTaskCandidateText::Name,
    }
}

pub(super) fn candidate_text_for_task(
    task: &task::TaskInfo,
    candidate_text: ProjectTaskCandidateText,
) -> String {
    match candidate_text {
        ProjectTaskCandidateText::Name => task.name.clone(),
        ProjectTaskCandidateText::NxRunArgument => task
            .command
            .strip_prefix("nx run ")
            .unwrap_or(&task.name)
            .to_string(),
    }
}

/// direnv walks the directory chain from cwd upward, so offer `.envrc`
/// files found there plus one level of subdirectories. Both the `.envrc`
/// path and its containing directory are offered because `direnv`
/// allow/deny/edit accept either form.
const DIRENV_RC_ANCESTOR_DEPTH_LIMIT: usize = 8;

fn load_direnv_rc_values(start: &Path) -> Vec<String> {
    let Ok(start) = start.canonicalize() else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for depth in 0..=DIRENV_RC_ANCESTOR_DEPTH_LIMIT {
        let Some(dir) = start.ancestors().nth(depth) else {
            break;
        };
        if dir.join(".envrc").is_file() {
            match depth {
                0 => {
                    values.push(".envrc".to_string());
                    values.push(".".to_string());
                }
                _ => {
                    let dir_text = vec![".."; depth].join("/");
                    values.push(format!("{dir_text}/.envrc"));
                    values.push(dir_text);
                }
            }
        }
        if dir.parent().is_none() {
            break;
        }
    }
    if let Ok(entries) = fs::read_dir(&start) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && path.join(".envrc").is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                values.push(format!("{name}/.envrc"));
                values.push(name.to_string());
            }
        }
    }
    dedup_sorted(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CommandLineParser;

    #[test]
    fn nx_run_scope_selects_nx_sources_and_argument_text() {
        let input = "nx run ";
        let parsed = CommandLineParser::new().parse(input, input.len());
        let config = completion_config(Some("nx.run"), &parsed).unwrap();

        assert_eq!(config.sources, NX_PROJECT_TASK_SOURCES);
        assert_eq!(
            config.candidate_text,
            ProjectTaskCandidateText::NxRunArgument
        );
    }

    #[test]
    fn load_direnv_rc_values_lists_envrc_and_containing_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".envrc"), "").unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".envrc"), "").unwrap();

        let values = load_direnv_rc_values(dir.path());
        for expected in [".envrc", ".", "sub", "sub/.envrc"] {
            assert!(
                values.contains(&expected.to_string()),
                "missing {expected} in {values:?}"
            );
        }
    }
}
