use super::super::super::Action;
use super::{get_ai_service, get_directory_listing, get_recent_commands};
use crate::ai_features;
use crate::shell::Shell;
use anyhow::Result;

use async_trait::async_trait;

pub struct SuggestCommandsAction;

#[async_trait(?Send)]
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

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let Some(service) = get_ai_service(shell) else {
            println!(
                "\r\nAI service is not configured. {}\r\n",
                dsh_openai::API_KEY_SETUP_HINT
            );
            return Ok(());
        };

        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let dir_listing = get_directory_listing();
        let recent_commands = get_recent_commands(shell, 5);

        super::run_and_render(
            service.as_ref(),
            ai_features::suggest_next_commands(
                service.as_ref(),
                &recent_commands,
                &cwd,
                &dir_listing,
            ),
        )
        .await
    }
}
