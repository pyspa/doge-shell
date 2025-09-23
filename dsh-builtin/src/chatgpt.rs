use super::ShellProxy;
use dsh_openai::{ChatGptClient, OpenAiConfig};
use dsh_types::{Context, ExitStatus};

/// Environment variable key for storing the chat prompt template
const PROMPT_KEY: &str = "CHAT_PROMPT";
/// Primary configuration key for storing the default model
const MODEL_KEY: &str = "AI_CHAT_MODEL";
/// Legacy key maintained for backwards compatibility with older configs
const LEGACY_MODEL_KEY: &str = "OPENAI_MODEL";

fn load_openai_config(proxy: &mut dyn ShellProxy) -> OpenAiConfig {
    OpenAiConfig::from_getter(|key| proxy.get_var(key).or_else(|| std::env::var(key).ok()))
}

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

    execute_chat_message(ctx, proxy, &message, model_override.as_deref())
}

/// Execute a chat request using the configured OpenAI client
pub fn execute_chat_message(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    message: &str,
    model_override: Option<&str>,
) -> ExitStatus {
    if message.trim().is_empty() {
        ctx.write_stderr("chat: message content required").ok();
        return ExitStatus::ExitedWith(1);
    }

    let config = load_openai_config(proxy);

    if config.api_key().is_none() {
        ctx.write_stderr("AI_CHAT_API_KEY / OPENAI_API_KEY not found")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    match ChatGptClient::try_from_config(&config) {
        Ok(client) => {
            let prompt = proxy.get_var(PROMPT_KEY);
            let model_override = model_override.map(|model| model.to_string());

            match client.send_message_with_model(message, prompt, Some(0.1), model_override) {
                Ok(res) => {
                    ctx.write_stdout(res.trim()).ok();
                    ExitStatus::ExitedWith(0)
                }
                Err(err) => {
                    ctx.write_stderr(&format!("\r{err:?}")).ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }
        Err(err) => {
            ctx.write_stderr(&format!("\r{err:?}")).ok();
            ExitStatus::ExitedWith(1)
        }
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
            // Show current model using resolved configuration
            let config = load_openai_config(proxy);
            let current_model = config.default_model().to_string();
            ctx.write_stdout(&format!("Current OpenAI model: {current_model}"))
                .ok();
            ExitStatus::ExitedWith(0)
        }
        2 => {
            // Set new model
            let new_model = &argv[1];
            proxy.set_var(MODEL_KEY.to_string(), new_model.to_string());
            proxy.set_var(LEGACY_MODEL_KEY.to_string(), new_model.to_string());
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
