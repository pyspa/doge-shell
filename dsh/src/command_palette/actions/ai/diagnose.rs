use super::super::super::Action;
use super::get_ai_service;
use crate::ai_features;
use crate::shell::Shell;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};

pub struct DiagnoseErrorAction;

impl Action for DiagnoseErrorAction {
    fn name(&self) -> &str {
        "Ai: Diagnose Last Error"
    }

    fn description(&self) -> &str {
        "Analyze the last command output to diagnose errors"
    }

    fn icon(&self) -> &str {
        "🔍"
    }

    fn category(&self) -> &str {
        "AI"
    }

    fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let Some(service) = get_ai_service(shell) else {
            println!("\r\nAI service not configured. Set OPENAI_API_KEY or AI_CHAT_API_KEY.\r\n");
            return Ok(());
        };

        // Get last output from environment
        let output = shell.environment.read().get_var("OUT").unwrap_or_default();

        // We need history to get the last command string
        let history = if let Some(ref history_arc) = shell.cmd_history {
            if let Some(history) = history_arc.try_lock() {
                history
                    .get_recent_context(1)
                    .first()
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if history.is_empty() {
            println!("\r\nNo recent command found to diagnose.\r\n");
            return Ok(());
        }

        let mut renderer = TerminalRenderer::new();
        queue!(renderer, Print("\r\n🔄 Processing...\r\n")).ok();
        renderer.flush().ok();

        let result = tokio::runtime::Handle::current().block_on(async {
            // We don't have exit code easily in Shell, assume failing if diagnosing?
            // Actually Repl had it. For now pass 1.
            ai_features::diagnose_output(service.as_ref(), &history, &output, 1).await
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
