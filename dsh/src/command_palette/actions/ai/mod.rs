use crate::ai_features::AiService;
use crate::shell::Shell;
use std::sync::Arc;

pub mod describe_dir;
pub mod diagnose;
pub mod explain;
pub mod safety;
pub mod suggest;
pub mod suggest_commands;

/// Get the AI service from the shell environment
pub fn get_ai_service(shell: &Shell) -> Option<Arc<dyn AiService + Send + Sync>> {
    shell
        .environment
        .read()
        .integration_state
        .ai_service
        .clone()
}

/// Helper to get directory listing for AI context
pub fn get_directory_listing() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let entries = crate::ai_features::directory_listing_entries(&cwd);
    if entries.is_empty() {
        return "Unable to read directory".to_string();
    }
    entries.join("\n")
}

/// Helper to get recent commands from history
pub fn get_recent_commands(shell: &Shell, count: usize) -> Vec<String> {
    if let Some(ref history_arc) = shell.cmd_history
        && let Some(history) = history_arc.try_lock()
    {
        return history.get_recent_context(count);
    }
    Vec::new()
}
