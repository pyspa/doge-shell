pub mod ai;
pub mod builtin_command;
pub mod clear_screen;
pub mod dashboard;
pub mod docker_containers;
pub mod find_file;
pub mod git_add;
pub mod git_checkout;
pub mod git_commit;
pub mod git_diff;
pub mod git_log;
pub mod git_push_pull;
pub mod git_stash;
pub mod jump_directory;
pub mod kill_process;
pub mod lisp_action;
pub mod port_check;
pub mod reload_config;
pub mod run_script;
pub mod search_history;
pub mod show_env;
pub mod ssh_connect;

pub use lisp_action::*;

use super::ActionRegistry;
use builtin_command::{BuiltinCommandAction, BuiltinInvocation};
use std::sync::Arc;

pub fn register_all(registry: &mut ActionRegistry) {
    // Dev
    registry.register(Arc::new(run_script::RunScriptAction));
    // Shell
    registry.register(Arc::new(clear_screen::ClearScreenAction));
    registry.register(Arc::new(reload_config::ReloadConfigAction));
    registry.register(Arc::new(dashboard::DashboardAction));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Doctor Setup",
        "Show setup diagnostics and next steps",
        "Setup",
        "doctor setup",
        "doctor fix, help doctor",
        "doctor",
        BuiltinInvocation::Static(&["setup"]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Doctor Fix",
        "Create safe missing setup files and directories",
        "Setup",
        "doctor fix",
        "doctor setup, help doctor",
        "doctor",
        BuiltinInvocation::Static(&["fix"]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Project Status",
        "Show current project onboarding status",
        "Project",
        "pm status",
        "pm init, task, doctor setup",
        "pm",
        BuiltinInvocation::Static(&["status"]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Project Init",
        "Register the current project root",
        "Project",
        "pm init",
        "pm status, pm activate",
        "pm",
        BuiltinInvocation::Static(&["init"]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Help: AI",
        "Show AI command help",
        "Help",
        "help ai",
        "safe-run, comp-gen, mcp",
        "help",
        BuiltinInvocation::Static(&["ai"]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Help: Project",
        "Show project command help",
        "Help",
        "help project",
        "pm, pj, task",
        "help",
        BuiltinInvocation::Static(&["project"]),
    )));
    // File
    registry.register(Arc::new(find_file::FindFileAction));
    // Navigation
    registry.register(Arc::new(search_history::SearchHistoryAction));
    registry.register(Arc::new(jump_directory::JumpDirectoryAction));
    // Process / System
    registry.register(Arc::new(kill_process::KillProcessAction));
    registry.register(Arc::new(port_check::PortCheckAction));
    // Git
    registry.register(Arc::new(git_add::GitAddAction));
    registry.register(Arc::new(git_checkout::GitCheckoutAction));
    registry.register(Arc::new(git_commit::GitCommitAction));
    registry.register(Arc::new(git_diff::GitDiffAction));
    registry.register(Arc::new(git_log::GitLogAction));
    registry.register(Arc::new(git_push_pull::GitPushPullAction));
    registry.register(Arc::new(git_stash::GitStashAction));
    // Docker
    registry.register(Arc::new(docker_containers::DockerContainersAction));
    // SSH
    registry.register(Arc::new(ssh_connect::SshConnectAction));
    // Environment
    registry.register(Arc::new(show_env::ShowEnvAction));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Manage Snippets",
        "List saved command snippets",
        "Shell",
        "snippet",
        "bookmark, help snippet",
        "snippet",
        BuiltinInvocation::Static(&[]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Manage Bookmarks",
        "List saved command bookmarks",
        "Shell",
        "bookmark",
        "snippet, history",
        "bookmark",
        BuiltinInvocation::Static(&[]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "List Output History",
        "List captured command outputs",
        "Shell",
        "out --list",
        "tm, history",
        "out",
        BuiltinInvocation::Static(&["--list"]),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Output History Search",
        "Search captured command outputs",
        "Shell",
        "tm",
        "out --list, history",
        "tm",
        BuiltinInvocation::Static(&[]),
    )));

    // AI
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Safe Run Current Input",
        "Analyze the current command before running it",
        "AI",
        "safe-run <current input>",
        "help safe-run",
        "safe-run",
        BuiltinInvocation::CurrentInputOrHelp("safe-run"),
    )));
    registry.register(Arc::new(BuiltinCommandAction::new(
        "Generate Completion",
        "Generate completion for the current command name",
        "AI",
        "comp-gen <current command>",
        "help comp-gen",
        "comp-gen",
        BuiltinInvocation::FirstInputTokenOrHelp("comp-gen"),
    )));
    registry.register(Arc::new(ai::explain::ExplainCommandAction));
    registry.register(Arc::new(ai::suggest::SuggestImprovementAction));
    registry.register(Arc::new(ai::safety::CheckSafetyAction));
    registry.register(Arc::new(ai::diagnose::DiagnoseErrorAction));
    registry.register(Arc::new(ai::describe_dir::DescribeDirectoryAction));
    registry.register(Arc::new(ai::suggest_commands::SuggestCommandsAction));
}
