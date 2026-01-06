use super::ShellProxy;
use crate::markdown::render_markdown_with_fallback;
use dsh_openai::{CANCELLED_MESSAGE, ChatGptClient, OpenAiConfig, is_ctrl_c_cancelled};
use dsh_types::{Context, ExitStatus};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::{Value, json};
use std::fs;
use std::process::Command;
use std::time::Duration;

/// Environment variable key for storing the chat prompt template
const PROMPT_KEY: &str = "CHAT_PROMPT";
/// Primary configuration key for storing the default model
const MODEL_KEY: &str = "AI_CHAT_MODEL";
/// Maximum number of iterations to satisfy tool calls before aborting
const MAX_TOOL_ITERATIONS: usize = 100;
/// System prompt that explains how to use the builtin tools
const TOOL_SYSTEM_PROMPT: &str = r#"You are DogeShell Assistant, a highly capable, autonomous DevOps and Software Engineering agent running directly inside the user's terminal (doge-shell). Your goal is to help the user perform tasks, fix issues, and write code efficiently and accurately.

## Operational Rules
1. **Plan first**: Before executing any tools, briefly analyze the request and outline your plan.
2. **Execute**: Use the provided tools to carry out your plan.
3. **Verify**: ALWAYS verify your actions.
   - If you create or edit a file, read it back to confirm the content is correct.
   - If you run a command, check its exit code and output.
4. **Analyze Errors**: If a tool fails (especially `execute`), DO NOT immediately ask the user for help.
   - Analyze the error message.
   - If it's a permission issue or a missing command, propose an alternative.
   - If a command is not on the allowlist, explain this constraint and ask the user to add it or try a different approach.

## Tools
You have access to the following tools:

- `execute`: Run shell commands.
  - **IMPORTANT**: This tool uses an allowlist. You might be blocked from running arbitrary commands. Check the error message carefully.
- `ls`: List files in a directory. Use this to explore the filesystem.
- `read`: Read the contents of a file.
- `edit`: Create or modify files.
- `search`: Search for string patterns in files (grep-like).

## Formatting
- Use Markdown for all methodology and code blocks.
- Be concise in your explanations but thorough in your verification.
"#;

mod mcp;
pub use mcp::{McpConnectionStatus, McpManager, McpServerStatus};
mod tool;

use tool::{build_tools, execute_tool_call};
pub fn load_openai_config(proxy: &mut dyn ShellProxy) -> OpenAiConfig {
    OpenAiConfig::from_getter(|key| proxy.get_var(key).or_else(|| std::env::var(key).ok()))
}

/// Built-in chat command description
pub fn chat_description() -> &'static str {
    "Chat with AI assistant"
}

/// Built-in chat command implementation
/// Integrates OpenAI API for AI-powered assistance within the shell
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
        ctx.write_stderr("AI_CHAT_API_KEY not found").ok();
        return ExitStatus::ExitedWith(1);
    }

    match ChatGptClient::try_from_config(&config) {
        Ok(client) => {
            let prompt = proxy.get_var(PROMPT_KEY);
            let model_override = model_override.map(|model| model.to_string());
            let mcp_manager = McpManager::load_blocking(proxy.list_mcp_servers());

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
                    let rendered = render_markdown_with_fallback(res.trim());
                    let trimmed = rendered.trim_end_matches('\n');
                    ctx.write_stdout(trimmed).ok();
                    ExitStatus::ExitedWith(0)
                }
                Err(err) if err == CANCELLED_MESSAGE => ExitStatus::ExitedWith(1),
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

/// Built-in chat_prompt command description
pub fn chat_prompt_description() -> &'static str {
    "Set or show the system prompt for chat"
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

/// Built-in chat_model command description
pub fn chat_model_description() -> &'static str {
    "Set or show the AI model used for chat"
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
    // 1. Static System Prompt (Cacheable)
    messages.push(json!({
        "role": "system",
        "content": build_system_prompt(operator_prompt, mcp_manager),
    }));

    // 2. Dynamic Context (Environment Snapshot)
    messages.push(json!({
        "role": "user",
        "content": build_dynamic_context(proxy),
    }));

    // 3. User Input
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
                // "Waiting for LLM response...".to_string()
                "".to_string()
            } else {
                //format!("Waiting for LLM response... (attempt {iterations})")
                "".to_string()
            };

            let _spinner = SpinnerGuard::start(&spinner_text);
            client
                .send_chat_request(
                    &messages,
                    temperature,
                    model_override.clone(),
                    Some(&tools),
                    Some(&|| proxy.is_canceled()),
                )
                .map_err(|err| {
                    if is_ctrl_c_cancelled(&err) {
                        err.to_string()
                    } else {
                        format!("{err:?}")
                    }
                })?
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

                let tool_result = match execute_tool_call(tool_call, mcp_manager, proxy) {
                    Ok(res) => res,
                    Err(err) => format!(
                        "Error: {err}\nPlease analyze the error and retry with corrected arguments."
                    ),
                };

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

fn build_dynamic_context(proxy: &mut dyn ShellProxy) -> String {
    format!("Environment snapshot:\n{}", environment_snapshot(proxy))
}

fn environment_snapshot(proxy: &mut dyn ShellProxy) -> String {
    let os_family = std::env::consts::FAMILY;
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("(failed to resolve current directory: {err})"));

    let mut aliases: Vec<_> = proxy.list_aliases().into_iter().collect();
    aliases.sort_by(|a, b| a.0.cmp(&b.0));

    let alias_str = if aliases.is_empty() {
        "none".to_string()
    } else {
        aliases
            .iter()
            .map(|(name, cmd)| format!("{name}='{cmd}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut files_str = String::new();
    if let Ok(entries) = fs::read_dir(".") {
        let names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    format!("{}/", name)
                } else {
                    name
                }
            })
            .take(50) // Limit to 50 files to avoid context bloating
            .collect();

        if !names.is_empty() {
            files_str = format!("\n- Visible files: {}", names.join(", "));
        }
    }

    format!(
        "- OS family: {os_family}\n- OS: {os}\n- Architecture: {arch}\n- Current directory: {cwd}\n- Git: {}{}- Aliases: {alias_str}",
        describe_git_state(),
        files_str
    )
}

fn describe_git_state() -> String {
    match Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let inside = String::from_utf8_lossy(&output.stdout)
                .trim()
                .eq_ignore_ascii_case("true");

            if !inside {
                return "not inside a Git worktree".to_string();
            }

            let root = git_string(["rev-parse", "--show-toplevel"]);
            let branch = git_branch_description();

            match root {
                Some(root) => format!("inside a Git worktree (root: {root}, {branch})"),
                None => format!("inside a Git worktree ({branch})"),
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                let code = output
                    .status
                    .code()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string());
                format!("unable to determine Git status (exit status {code})")
            } else {
                format!("unable to determine Git status ({})", stderr.trim())
            }
        }
        Err(err) => format!("git command unavailable ({err})"),
    }
}

fn git_branch_description() -> String {
    match git_string(["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(name) if name == "HEAD" => git_string(["rev-parse", "--short", "HEAD"])
            .map(|commit| format!("detached at {commit}"))
            .unwrap_or_else(|| "detached HEAD".to_string()),
        Some(name) => format!("branch {name}"),
        None => "branch unknown".to_string(),
    }
}

fn git_string<const N: usize>(args: [&str; N]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn extract_message_content(message: &Value) -> Option<String> {
    let content = message.get("content")?;

    let mut segments = Vec::new();
    collect_text_segments(content, &mut segments);

    let combined = segments.join("");
    if combined.trim().is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn collect_text_segments(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !text.is_empty() {
                out.push(text.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_text_segments(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text") {
                collect_text_segments(text, out);
            }
            if let Some(content) = map.get("content") {
                collect_text_segments(content, out);
            }
            if let Some(value_field) = map.get("value") {
                collect_text_segments(value_field, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_plain_string_content() {
        let message = json!({
            "content": "Hello world",
        });

        assert_eq!(
            extract_message_content(&message),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn extract_array_of_text_segments() {
        let message = json!({
            "content": [
                {"text": "First"},
                {"content": "Second"},
            ],
        });

        assert_eq!(
            extract_message_content(&message),
            Some("FirstSecond".to_string())
        );
    }

    #[test]
    fn extract_nested_value_field() {
        let message = json!({
            "content": [
                {
                    "type": "text",
                    "text": {
                        "value": "概要を説明します",
                        "annotations": []
                    }
                }
            ],
        });

        assert_eq!(
            extract_message_content(&message),
            Some("概要を説明します".to_string())
        );
    }

    #[test]
    fn returns_none_for_whitespace_only() {
        let message = json!({
            "content": [
                {"text": "   \n"},
            ],
        });

        assert_eq!(extract_message_content(&message), None);
    }
}
