use super::ShellProxy;
use dsh_openai::ChatGptClient;
use dsh_types::{Context, ExitStatus};

/// Environment variable key for storing the chat prompt template
const PROMPT_KEY: &str = "CHAT_PROMPT";
/// Environment variable key for storing the default OpenAI model
const MODEL_KEY: &str = "OPENAI_MODEL";

/// Built-in chat command implementation
/// Integrates OpenAI ChatGPT API for AI-powered assistance within the shell
/// Requires OPENAI_API_KEY environment variable to be set
///
/// Usage:
///   chat "message"                    - Use default model
///   chat -m model "message"           - Use specific model
///   chat --model model "message"      - Use specific model (long form)
pub fn chat(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        ctx.write_stderr("Usage: chat [-m|--model <model>] <message>")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    // Parse arguments for model override and message content
    let (message, model_override) = match parse_chat_args(&argv) {
        Ok(result) => result,
        Err(err) => {
            ctx.write_stderr(&format!("chat: {err}")).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // Check if OpenAI API key is available
    if let Some(key) = proxy.get_var("OPENAI_API_KEY") {
        // Get default model from environment variable
        let default_model = proxy.get_var(MODEL_KEY);

        match ChatGptClient::new_with_model(key, default_model) {
            Ok(client) => {
                // Get optional custom prompt template
                let prompt = proxy.get_var(PROMPT_KEY);

                // Send message to ChatGPT with low temperature for consistent responses
                match client.send_message_with_model(&message, prompt, Some(0.1), model_override) {
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
        ctx.write_stderr("OPENAI_API_KEY not found").ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Built-in chat_prompt command implementation
/// Sets a custom prompt template for ChatGPT interactions
/// The prompt template is used to provide context for all subsequent chat commands
pub fn chat_prompt(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        // Require prompt text as argument
        ctx.write_stderr("Usage: chat_prompt <prompt_text>").ok();
        ExitStatus::ExitedWith(1)
    } else {
        let prompt = &argv[1];
        // Store the prompt template in shell variables
        proxy.set_var(PROMPT_KEY.to_string(), prompt.to_string());
        ctx.write_stdout(&format!("Chat prompt set to: {prompt}"))
            .ok();
        ExitStatus::ExitedWith(0)
    }
}

/// Built-in chat_model command implementation
/// Manages the default OpenAI model for ChatGPT interactions
///
/// Usage:
///   chat_model                - Show current default model
///   chat_model <model>        - Set default model
pub fn chat_model(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match argv.len() {
        1 => {
            // Show current model
            let current_model = proxy
                .get_var(MODEL_KEY)
                .unwrap_or_else(|| "o1-mini (default)".to_string());
            ctx.write_stdout(&format!("Current OpenAI model: {current_model}"))
                .ok();
            ExitStatus::ExitedWith(0)
        }
        2 => {
            // Set new model
            let new_model = &argv[1];
            proxy.set_var(MODEL_KEY.to_string(), new_model.to_string());
            ctx.write_stdout(&format!("OpenAI model set to: {new_model}"))
                .ok();
            ExitStatus::ExitedWith(0)
        }
        _ => {
            ctx.write_stderr("Usage: chat_model [model_name]").ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Parse chat command arguments to extract message and optional model override
/// Returns (message, model_override)
fn parse_chat_args(argv: &[String]) -> Result<(String, Option<String>), String> {
    let mut i = 1;
    let mut model_override = None;

    // Parse options
    while i < argv.len() {
        match argv[i].as_str() {
            "-m" | "--model" => {
                if i + 1 >= argv.len() {
                    return Err("model option requires a value".to_string());
                }
                model_override = Some(argv[i + 1].clone());
                i += 2;
            }
            _ => break, // First non-option argument is the message
        }
    }

    // Get message content
    if i >= argv.len() {
        return Err("message content required".to_string());
    }

    let message = argv[i].clone();
    Ok((message, model_override))
}
