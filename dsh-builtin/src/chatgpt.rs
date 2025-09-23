use super::ShellProxy;
use dsh_openai::{ChatGptClient, OpenAiConfig};
use dsh_types::{Context, ExitStatus};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::{Value, json};
use std::time::Duration;

/// Environment variable key for storing the chat prompt template
const PROMPT_KEY: &str = "CHAT_PROMPT";
/// Primary configuration key for storing the default model
const MODEL_KEY: &str = "AI_CHAT_MODEL";
/// Legacy key maintained for backwards compatibility with older configs
const LEGACY_MODEL_KEY: &str = "OPENAI_MODEL";
/// Maximum number of iterations to satisfy tool calls before aborting
const MAX_TOOL_ITERATIONS: usize = 6;
/// System prompt that explains how to use the builtin tools
const TOOL_SYSTEM_PROMPT: &str = r#"You are the AI assistant that runs inside doge-shell. You may update files by calling the `edit` tool. When you need to change a file, call the tool with JSON arguments:
{
  "path": "relative/path/to/file",
  "contents": "entire desired file contents"
}
The `path` must stay inside the workspace (no absolute paths or `..`). Always supply the full file contents; the tool overwrites the file.

You can run allowed shell commands via the `execute` tool when the operator wants real command output:
{
  "command": "ls -la"
}
Only invoke `execute` with commands whose first token is present in the configured allowlist (see ~/.config/dsh/openai-execute-tool.json or AI_CHAT_EXECUTE_ALLOWLIST). Never fabricate command results and never attempt disallowed programs. After running a command, summarize the observed stdout/stderr and note any failures in your reply.

If a tool call is not needed, answer normally. After you finish applying edits or command runs, describe the changes in your final reply. Only call tools when a real action is required."#;

mod mcp;
mod tool;

use mcp::McpManager;
use tool::{build_tools, execute_tool_call};
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
            let mcp_manager = McpManager::load(proxy.list_mcp_servers());

            match chat_with_tools(
                &client,
                message,
                prompt,
                Some(0.1),
                model_override,
                &mcp_manager,
                proxy,
            ) {
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

fn chat_with_tools(
    client: &ChatGptClient,
    user_input: &str,
    operator_prompt: Option<String>,
    temperature: Option<f64>,
    model_override: Option<String>,
    mcp_manager: &McpManager,
    proxy: &mut dyn ShellProxy,
) -> Result<String, String> {
    let mut messages = Vec::new();
    messages.push(json!({
        "role": "system",
        "content": build_system_prompt(operator_prompt, mcp_manager),
    }));
    messages.push(json!({ "role": "user", "content": user_input }));

    let mut tools = build_tools();
    if !mcp_manager.is_empty() {
        tools.extend(mcp_manager.tool_definitions());
    }
    let mut iterations = 0;

    loop {
        iterations += 1;
        if iterations > MAX_TOOL_ITERATIONS {
            return Err("chat: exceeded maximum number of tool interactions".to_string());
        }

        let response = {
            let spinner_text = if iterations == 1 {
                "Waiting for LLM response...".to_string()
            } else {
                format!("Waiting for LLM response... (attempt {iterations})")
            };

            let _spinner = SpinnerGuard::start(&spinner_text);
            client
                .send_chat_request(&messages, temperature, model_override.clone(), Some(&tools))
                .map_err(|err| format!("{err:?}"))?
        };

        let choice = response
            .get("choices")
            .and_then(|choices| choices.get(0))
            .ok_or_else(|| format!("chat: unexpected response structure {response}"))?;

        let assistant_message = choice
            .get("message")
            .cloned()
            .ok_or_else(|| format!("chat: response missing assistant message {response}"))?;

        messages.push(assistant_message.clone());

        if let Some(tool_calls) = assistant_message
            .get("tool_calls")
            .and_then(|v| v.as_array())
        {
            if tool_calls.is_empty() {
                continue;
            }

            for tool_call in tool_calls {
                let tool_call_id = tool_call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let tool_result = execute_tool_call(tool_call, mcp_manager, proxy)?;

                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": tool_result,
                }));
            }

            continue;
        }

        if choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            == "tool_calls"
        {
            continue;
        }

        let content = extract_message_content(&assistant_message)
            .ok_or_else(|| format!("chat: assistant returned empty content {response}"))?;

        return Ok(content);
    }
}

struct SpinnerGuard {
    progress: ProgressBar,
}

impl SpinnerGuard {
    fn start(message: &str) -> Self {
        let progress = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_chars("-\\|/");
        progress.set_style(style);
        progress.set_message(message.to_string());
        progress.enable_steady_tick(Duration::from_millis(80));
        SpinnerGuard { progress }
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.progress.finish_and_clear();
    }
}

fn build_system_prompt(operator_prompt: Option<String>, mcp_manager: &McpManager) -> String {
    let mut base = TOOL_SYSTEM_PROMPT.to_string();

    if let Some(fragment) = mcp_manager.system_prompt_fragment() {
        base.push_str("\n\nMCP access:");
        base.push('\n');
        base.push_str(&fragment);
    }

    match operator_prompt.and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }) {
        Some(extra) => {
            base.push_str("\n\nAdditional operator instructions:\n");
            base.push_str(&extra);
            base
        }
        None => base,
    }
}

fn extract_message_content(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => Some(text.to_string()),
        Some(Value::Array(items)) => {
            let mut buffer = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    buffer.push_str(text);
                } else if let Some(text) = item.get("content").and_then(|v| v.as_str()) {
                    buffer.push_str(text);
                }
            }

            if buffer.is_empty() {
                None
            } else {
                Some(buffer)
            }
        }
        _ => None,
    }
}
