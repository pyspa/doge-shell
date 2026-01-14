use super::super::super::Action;
use super::{get_ai_service, get_directory_listing, get_recent_commands};
use crate::ai_features;
use crate::shell::Shell;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};

pub struct SuggestCommandsAction;

impl Action for SuggestCommandsAction {
    fn name(&self) -> &str {
        "Ai: Suggest Commands"
    }

    fn description(&self) -> &str {
        "Suggest useful commands based on current context"
    }

    fn icon(&self) -> &str {
        "💡"
    }

    fn category(&self) -> &str {
        "AI"
    }

    fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let Some(service) = get_ai_service(shell) else {
            println!("\r\nAI service not configured. Set OPENAI_API_KEY or AI_CHAT_API_KEY.\r\n");
            return Ok(());
        };

        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let dir_listing = get_directory_listing();
        let recent_commands = get_recent_commands(shell, 5);

        let mut renderer = TerminalRenderer::new();
        queue!(renderer, Print("\r\n🔄 Processing...\r\n")).ok();
        renderer.flush().ok();

        let result = tokio::runtime::Handle::current().block_on(async {
            ai_features::suggest_next_commands(
                service.as_ref(),
                &recent_commands,
                &cwd,
                &dir_listing,
            )
            .await
        });

        match result {
            Ok(response) => {
                queue!(renderer, Print("\r")).ok();
                queue!(renderer, Clear(ClearType::CurrentLine)).ok();
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
}
