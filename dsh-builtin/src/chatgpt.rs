use super::ShellProxy;
use dsh_openai::ChatGptClient;
use dsh_types::{Context, ExitStatus};

/// Environment variable key for storing the chat prompt template
const PROMPT_KEY: &str = "CHAT_PROMPT";

/// Built-in chat command implementation
/// Integrates OpenAI ChatGPT API for AI-powered assistance within the shell
/// Requires OPEN_AI_API_KEY environment variable to be set
pub fn chat(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if OpenAI API key is available
    if let Some(key) = proxy.get_var("OPEN_AI_API_KEY") {
        match ChatGptClient::new(key) {
            Ok(client) => {
                let content = &argv[1];
                // Get optional custom prompt template
                let prompt = proxy.get_var(PROMPT_KEY);

                // Send message to ChatGPT with low temperature for consistent responses
                match client.send_message(content, prompt, Some(0.1)) {
                    Ok(res) => {
                        // Output ChatGPT response to stdout
                        ctx.write_stdout(res.trim()).ok();
                        ExitStatus::ExitedWith(0)
                    }
                    Err(err) => {
                        // Report API communication errors
                        ctx.write_stderr(&format!("\r{err:?}")).ok();
                        ExitStatus::ExitedWith(1)
                    }
                }
            }
            Err(err) => {
                // Report client initialization errors
                ctx.write_stderr(&format!("\r{err:?}")).ok();
                ExitStatus::ExitedWith(1)
            }
        }
    } else {
        // API key not found - inform user of requirement
        ctx.write_stderr("OPEN_AI_API_KEY not found").ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Built-in chat_prompt command implementation
/// Sets a custom prompt template for ChatGPT interactions
/// The prompt template is used to provide context for all subsequent chat commands
pub fn chat_prompt(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        // Require prompt text as argument
        ctx.write_stderr("chat_prompt variable").ok();
    } else {
        let prompt = &argv[1];
        // Store the prompt template in shell variables
        proxy.set_var(PROMPT_KEY.to_string(), prompt.to_string());
    }
    ExitStatus::ExitedWith(0)
}
