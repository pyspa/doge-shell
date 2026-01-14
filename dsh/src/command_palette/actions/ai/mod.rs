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
    shell.environment.read().ai_service.clone()
}

/// Helper to get directory listing for AI context
pub fn get_directory_listing() -> String {
    match std::fs::read_dir(".") {
        Ok(entries) => {
            let mut filestats: Vec<(String, bool)> = entries
                .filter_map(|e| e.ok())
                .take(30)
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    (name, is_dir)
                })
                .collect();
            filestats.sort_by(|a, b| a.0.cmp(&b.0));

            let mut lines = Vec::new();
            for (name, is_dir) in filestats {
                if is_dir {
                    lines.push(format!("{}/", name));
                } else {
                    lines.push(name);
                }
            }
            lines.join("\n")
        }
        Err(_) => "Unable to read directory".to_string(),
    }
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
