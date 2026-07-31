use super::{
    DENO_PROJECT_TASK_SOURCES, GRADLE_PROJECT_TASK_SOURCES, JS_PROJECT_TASK_SOURCES,
    JUST_PROJECT_TASK_SOURCES, MAKE_PROJECT_TASK_SOURCES, MISE_PROJECT_TASK_SOURCES,
    NX_PROJECT_TASK_SOURCES, ProjectTaskCandidateText, ProjectTaskCompletionConfig,
    TASKFILE_PROJECT_TASK_SOURCES, TURBO_PROJECT_TASK_SOURCES,
};
use crate::completion::parser::ParsedCommandLine;
use dsh_builtin::task;

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

pub(super) fn candidate_text(
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
