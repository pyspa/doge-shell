pub mod ai;
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
use std::sync::Arc;

pub fn register_all(registry: &mut ActionRegistry) {
    // Dev
    registry.register(Arc::new(run_script::RunScriptAction));
    // Shell
    registry.register(Arc::new(clear_screen::ClearScreenAction));
    registry.register(Arc::new(reload_config::ReloadConfigAction));
    registry.register(Arc::new(dashboard::DashboardAction));
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

    // AI
    registry.register(Arc::new(ai::explain::ExplainCommandAction));
    registry.register(Arc::new(ai::suggest::SuggestImprovementAction));
    registry.register(Arc::new(ai::safety::CheckSafetyAction));
    registry.register(Arc::new(ai::diagnose::DiagnoseErrorAction));
    registry.register(Arc::new(ai::describe_dir::DescribeDirectoryAction));
    registry.register(Arc::new(ai::suggest_commands::SuggestCommandsAction));
}
