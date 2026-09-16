use super::super::super::Action;
use super::get_ai_service;
use crate::ai_features;
use crate::shell::Shell;
use anyhow::Result;

use async_trait::async_trait;

pub struct DiagnoseErrorAction;

#[async_trait(?Send)]
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

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let Some(service) = get_ai_service(shell) else {
            println!(
                "\r\nAI service is not configured. {}\r\n",
                dsh_openai::API_KEY_SETUP_HINT
            );
            return Ok(());
        };

        // The palette has no REPL state, so the session's command blocks are
        // the only place the real command, exit code and both output streams
        // live. Reading `$OUT` and assuming exit code 1 - what this did before
        // - diagnosed the wrong thing whenever the error went to stderr.
        let failure = crate::ai_features::resolve_last_failure(&shell.environment.read(), None);

        let Some(failure) = failure.filter(|failure| !failure.command.is_empty()) else {
            println!("\r\nNo recent command found to diagnose.\r\n");
            return Ok(());
        };

        super::run_and_render(
            service.as_ref(),
            ai_features::diagnose_output(
                service.as_ref(),
                &failure.command,
                &failure.output,
                failure.exit_code,
            ),
        )
        .await
    }
}
