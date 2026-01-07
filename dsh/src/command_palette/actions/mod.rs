pub mod clear_screen;
pub mod dashboard;
pub mod find_file;
pub mod git_add;
pub mod git_checkout;
pub mod git_log;
pub mod git_stash;
pub mod jump_directory;
pub mod kill_process;
pub mod lisp_action;
pub mod reload_config;
pub mod search_history;

pub use lisp_action::*;

use super::ActionRegistry;
use std::sync::Arc;

pub fn register_all(registry: &mut ActionRegistry) {
    registry.register(Arc::new(clear_screen::ClearScreenAction));
    registry.register(Arc::new(reload_config::ReloadConfigAction));
    registry.register(Arc::new(git_add::GitAddAction));
    registry.register(Arc::new(git_checkout::GitCheckoutAction));
    registry.register(Arc::new(git_log::GitLogAction));
    registry.register(Arc::new(git_stash::GitStashAction));
    registry.register(Arc::new(dashboard::DashboardAction));
    registry.register(Arc::new(find_file::FindFileAction));
    registry.register(Arc::new(search_history::SearchHistoryAction));
    registry.register(Arc::new(jump_directory::JumpDirectoryAction));
    registry.register(Arc::new(kill_process::KillProcessAction));
}
