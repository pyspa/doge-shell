//! Project- and shell-state completion: task runner targets (npm/cargo/just/
//! make/...), `direnv`'s `.envrc`, ssh hosts, man pages, archive entries
//! (`tar`/`unzip`), and the shell's own aliases/abbreviations/env vars.
use super::{
    CachePolicy, CandidateType, CommandQueryPolicy, DENO_PROJECT_TASK_SOURCES,
    DynamicCompletionProvider, EnhancedCandidate, GRADLE_PROJECT_TASK_SOURCES,
    JS_PROJECT_TASK_SOURCES, JUST_PROJECT_TASK_SOURCES, MAKE_PROJECT_TASK_SOURCES,
    MISE_PROJECT_TASK_SOURCES, NX_PROJECT_TASK_SOURCES, ProjectTaskCandidateText,
    ProjectTaskCompletionConfig, TASKFILE_PROJECT_TASK_SOURCES, TURBO_PROJECT_TASK_SOURCES,
    archive_file_candidates, canonicalize_path, dedup_sorted, format_ssh_host_candidate_text,
    format_task_description, load_man_page_names, load_ssh_hosts, man_page_roots, matches_prefix,
    run_command_lines, selected_tar_archive, selected_unzip_archive, shell_state_candidates,
    ssh_config_scope, tar_reads_archive,
};
use crate::completion::parser::{CompletionContext, ParsedCommandLine};
use dsh_builtin::task;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

/// This family's rows for `local::collect` - see `local` for what belongs
/// here. Table only; routing is unaffected by which family's table a
/// provider's row lives in.
pub(super) const LOCAL_SPECS: &[super::local::LocalSpec] = &[super::local::LocalSpec {
    provider: "direnv.rc",
    command_name: "direnv",
    value_kind: "rc",
    scope: super::local::Scope::CurrentDir,
    source: super::local::Source::ScopePath {
        loader: load_direnv_rc_values,
    },
    description: ".envrc file",
}];

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

impl DynamicCompletionProvider {
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

    pub(crate) fn collect_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let current_token = parsed_command_line.current_token.as_str();
        let tasks = if cached_only {
            self.lookup_project_tasks(current_dir)
        } else {
            match self.load_project_tasks(current_dir) {
                Ok(tasks) => tasks,
                Err(e) => {
                    warn!("Failed to load task completions: {}", e);
                    return Vec::new();
                }
            }
        };

        tasks
            .into_iter()
            .filter(|task| matches_prefix(current_token, &task.name))
            .map(|task| EnhancedCandidate {
                text: task.name,
                description: Some(format_task_description(&task.source, &task.command)),
                candidate_type: CandidateType::Argument,
                priority: 90,
            })
            .collect()
    }
    fn collect_shell_alias_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        let values = self
            .environment
            .read()
            .variable_state
            .alias
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        shell_state_candidates(values, current_token, "shell alias")
    }
    fn collect_shell_abbr_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        let values = self
            .environment
            .read()
            .variable_state
            .abbreviations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        shell_state_candidates(values, current_token, "shell abbreviation")
    }
    fn collect_shell_env_var_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        let values = self
            .environment
            .read()
            .variable_state
            .variables
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        shell_state_candidates(values, current_token, "environment variable")
    }
    pub(crate) fn collect_ssh_host_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        _current_dir: &Path,
        command_name: &str,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        let current_token = parsed_command_line.current_token.as_str();
        if current_token.contains(':') {
            return Vec::new();
        }

        let scope = ssh_config_scope();
        let loader = move || Ok(load_ssh_hosts());
        let values = self.load_or_lookup_command_values(
            command_name,
            "ssh-host",
            scope,
            cached_only,
            CommandQueryPolicy::LOCAL,
            loader,
        );
        let user_prefix = current_token
            .rsplit_once('@')
            .map(|(user, _)| user.to_string());
        let host_token = current_token
            .rsplit_once('@')
            .map_or(current_token, |(_, host)| host);

        values
            .into_iter()
            .filter(|host| matches_prefix(host_token, host))
            .map(|host| {
                let text =
                    format_ssh_host_candidate_text(command_name, user_prefix.as_deref(), host);
                EnhancedCandidate {
                    text,
                    description: Some("ssh host".to_string()),
                    candidate_type: CandidateType::Argument,
                    priority: 130,
                }
            })
            .collect()
    }
    fn collect_man_page_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let configured_manpath = self.environment.read().get_var("MANPATH");
        let roots = man_page_roots(configured_manpath.as_deref());
        let scope = roots
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/usr/share/man"));
        self.collect_cached_value_candidates(
            "man",
            "page",
            scope,
            current_token,
            "manual page",
            cached_only,
            move || Ok(load_man_page_names(&roots)),
        )
    }
    fn collect_archive_entry_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_name = parsed_command_line.command.as_str();
        let archive = match command_name {
            "tar" if tar_reads_archive(parsed_command_line) => {
                selected_tar_archive(parsed_command_line, current_dir)
            }
            "unzip" => selected_unzip_archive(parsed_command_line, current_dir),
            _ => None,
        };

        let Some(archive) = archive else {
            if cached_only {
                return Vec::new();
            }
            return archive_file_candidates(parsed_command_line.current_token.as_str());
        };

        let command_path = self.resolve_command_path(command_name);
        let archive_arg = archive.to_string_lossy().to_string();
        let current_dir = current_dir.to_path_buf();
        let executable = if command_name == "tar" {
            "tar"
        } else {
            "unzip"
        };
        self.collect_cached_value_candidates(
            executable,
            "archive-entry",
            canonicalize_path(&archive),
            parsed_command_line.current_token.as_str(),
            "archive entry",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let args = if executable == "tar" {
                    vec!["-tf", archive_arg.as_str()]
                } else {
                    vec!["-Z1", archive_arg.as_str()]
                };
                run_command_lines(&command_path, &args, &current_dir)
            },
        )
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
