use super::super::super::Action;
use super::get_ai_service;
use crate::ai_features;
use crate::shell::Shell;
use anyhow::Result;

use async_trait::async_trait;

pub struct SuggestImprovementAction;

#[async_trait(?Send)]
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

    async fn execute(&self, shell: &mut Shell, input: &str) -> Result<()> {
        if input.trim().is_empty() {
            println!("\r\nNo command to improve.\r\n");
            return Ok(());
        }

        let Some(service) = get_ai_service(shell) else {
            println!(
                "\r\nAI service is not configured. {}\r\n",
                dsh_openai::API_KEY_SETUP_HINT
            );
            return Ok(());
        };

        super::run_and_render(
            service.as_ref(),
            ai_features::suggest_improvement(service.as_ref(), input),
        )
        .await
    }
}
