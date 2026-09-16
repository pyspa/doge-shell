use crate::ai_features::{self, AiService};
use crate::shell::Shell;
use anyhow::Result;
use crossterm::queue;
use crossterm::style::Print;
use std::future::Future;
use std::sync::Arc;

use crate::terminal::renderer::TerminalRenderer;

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

/// Run one AI action's request and print what comes back.
///
/// Every action in this directory had the same body: a static
/// "🔄 Processing..." line, an `.await` that made the terminal deaf for as
/// long as the provider took, and a loop printing the answer. The wait now
/// goes through [`ai_features::await_with_progress`], so the elapsed time is
/// visible and Esc gives up on it - the palette runs inside the REPL's key
/// handling, which is exactly where that helper is meant to be used.
pub(super) async fn run_and_render(
    service: &dyn AiService,
    fut: impl Future<Output = Result<String>>,
) -> Result<()> {
    let mut renderer = TerminalRenderer::new();
    queue!(renderer, Print("\r\n")).ok();
    renderer.flush().ok();

    // `None` is the user having pressed Esc; `await_with_progress` has already
    // said so and cancelled the request.
    let Some(result) = ai_features::await_with_progress("🔄 Processing...", service, fut).await
    else {
        return Ok(());
    };

    match result {
        Ok(response) => {
            for line in response.lines() {
                queue!(renderer, Print(format!("{}\r\n", line))).ok();
            }
            queue!(renderer, Print("\r\n")).ok();
        }
        Err(e) => {
            queue!(renderer, Print(format!("❌ Error: {}\r\n", e))).ok();
        }
    }
    renderer.flush().ok();

    Ok(())
}
