use super::super::super::Action;
use super::get_ai_service;
use crate::ai_features;
use crate::shell::Shell;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};

pub struct SuggestImprovementAction;

impl Action for SuggestImprovementAction {
    fn name(&self) -> &str {
        "Ai: Suggest Improvement"
    }

    fn description(&self) -> &str {
        "Suggest a more efficient version of the current command"
    }

    fn icon(&self) -> &str {
        "✨"
    }

    fn category(&self) -> &str {
        "AI"
    }

    fn execute(&self, shell: &mut Shell, input: &str) -> Result<()> {
        if input.trim().is_empty() {
            println!("\r\nNo command to improve.\r\n");
            return Ok(());
        }

        let Some(service) = get_ai_service(shell) else {
            println!("\r\nAI service not configured. Set OPENAI_API_KEY or AI_CHAT_API_KEY.\r\n");
            return Ok(());
        };

        let mut renderer = TerminalRenderer::new();
        queue!(renderer, Print("\r\n🔄 Processing...\r\n")).ok();
        renderer.flush().ok();

        let result = tokio::runtime::Handle::current()
            .block_on(async { ai_features::suggest_improvement(service.as_ref(), input).await });

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
