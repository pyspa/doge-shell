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
/// Environment variable key for storing the AI response language
const LANGUAGE_KEY: &str = "AI_MESSAGE_LANG";
/// Maximum number of iterations to satisfy tool calls before aborting
const MAX_TOOL_ITERATIONS: usize = 100;
/// Threshold of characters in the buffer to trigger summarization (~3k-12k tokens)
const MAX_BUFFER_CHARS: usize = 96000;
/// Environment variable key to override the model used for summarization
const SUMMARY_MODEL_KEY: &str = "AI_SUMMARY_MODEL";
const MAX_VISIBLE_FILES: usize = 12;
const MAX_VISIBLE_ALIASES: usize = 8;

/// System prompt that explains how to use the builtin tools
const TOOL_SYSTEM_PROMPT: &str = r#"You are DogeShell Assistant, an autonomous software engineering agent running inside doge-shell.

Rules:
1. Briefly plan before using tools.
2. Explore cheaply first: prefer `search` and `ls`; use `read_file` only after locating the exact target.
3. Verify every change. After `edit`, read the file back. After `execute`, check exit code, stdout, and stderr.
4. If a tool fails, analyze the error before asking the user. Respect the `execute` allowlist.

Tools:
- `search`: find files or matching text
- `ls`: inspect directories
- `read_file`: read a file
- `edit`: overwrite a file with exact contents
- `execute`: run an allowlisted command without shell evaluation

Respond in Markdown. Be concise and avoid unnecessary repetition.
"#;

struct ConversationManager {
    summary: Option<String>,
    buffer: Vec<Value>,
    buffer_chars: usize,
    /// System prompt (fixed) - index 0
    /// First user message (pinned) - index 1
    pinned_messages: Vec<Value>,
}

impl ConversationManager {
    fn new(system_prompt: Value, first_user_message: Value) -> Self {
        Self {
            summary: None,
            buffer: Vec::new(),
            buffer_chars: 0,
            pinned_messages: vec![system_prompt, first_user_message],
        }
    }

    fn add_message(&mut self, message: Value) {
        self.buffer_chars += message_serialized_len(&message);
        self.buffer.push(message);
    }

    fn buffer_size_chars(&self) -> usize {
        self.buffer_chars
    }

    fn should_summarize(&self) -> bool {
        self.buffer_size_chars() > MAX_BUFFER_CHARS
    }

    fn perform_summary(
        &mut self,
        client: &ChatGptClient,
        proxy: &mut dyn ShellProxy,
        model_override: Option<String>,
    ) -> Result<(), String> {
        let _spinner = SpinnerGuard::start("Summarizing conversation history...");

        // Determine which model to use for summarization:
        // 1. Check for AI_SUMMARY_MODEL environment variable
        // 2. Fall back to the main chat model (model_override or default)
        let summary_model = proxy
            .get_var(SUMMARY_MODEL_KEY)
            .or_else(|| std::env::var(SUMMARY_MODEL_KEY).ok())
            .or(model_override);

        let mut summary_messages = Vec::new();
        summary_messages.push(json!({
            "role": "system",
            "content": "You are a conversation summarizer. Your task is to update the summary of a technical conversation between a user and an AI DevOps agent. 
            
            Inputs:
            1. Current Summary (if any)
            2. Recent Messages (to be summarized)

            Output:
            A single, concise paragraph summarizing the entire history including the new messages. 
            - PRESERVE key technical details: file names, function names, error messages, and what actions were taken.
            - OMIT trivial chatter.
            - FOCUS on the state of the system and the progress of the task."
        }));

        let current_summary_text = self.summary.as_deref().unwrap_or("None");
        let buffer_text = self
            .buffer
            .iter()
            .map(|msg| {
                let role = msg
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let content = extract_message_content(msg).unwrap_or_default();

                // Include tool_calls information if present
                let tool_calls_desc = msg
                    .get("tool_calls")
                    .and_then(|tc| tc.as_array())
                    .map(|calls| {
                        let tool_names: Vec<String> = calls
                            .iter()
                            .filter_map(|c| {
                                let name = c
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())?;
                                let args = c
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("{}");
                                Some(format!("{name}({args})"))
                            })
                            .collect();
                        if tool_names.is_empty() {
                            String::new()
                        } else {
                            format!(" [Called: {}]", tool_names.join(", "))
                        }
                    })
                    .unwrap_or_default();

                format!("{role}: {content}{tool_calls_desc}")
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        summary_messages.push(json!({
            "role": "user",
            "content": format!("Current Summary:\n{current_summary_text}\n\nRecent Messages to Integrate:\n{buffer_text}")
        }));

        // Send request to summarization model
        let response = client
            .send_chat_request(
                &summary_messages,
                Some(0.3), // Lower temperature for consistent summarization
                summary_model,
                None, // No tools for summarizer
                Some(&|| proxy.is_canceled()),
            )
            .map_err(|e| format!("Summarization failed: {e:?}"))?;

        let choice = response
            .get("choices")
            .and_then(|choices| choices.get(0))
            .ok_or_else(|| "Summarization response missing choices".to_string())?;

        let message = choice
            .get("message")
            .cloned()
            .ok_or_else(|| "Summarization response missing message".to_string())?;

        let new_summary = extract_message_content(&message)
            .ok_or_else(|| "Summarization returned empty content".to_string())?;

        // Update state: keep most recent messages to maintain tool_call/result continuity
        const RETAIN_AFTER_SUMMARY: usize = 6; // Keep last ~3 exchanges (assistant+tool pairs)
        let retain_start = self.buffer.len().saturating_sub(RETAIN_AFTER_SUMMARY);
        self.buffer = self.buffer.split_off(retain_start);
        self.buffer_chars = sum_message_lengths(&self.buffer);
        self.summary = Some(new_summary);

        Ok(())
    }

    fn build_messages_for_chat(&self, dynamic_context: Value) -> Vec<Value> {
        let mut messages = Vec::new();

        // System prompt (index 0)
        messages.push(self.pinned_messages[0].clone());

        // Fresh dynamic context (regenerated each call)
        messages.push(dynamic_context);

        // First user message (index 1, pinned)
        messages.push(self.pinned_messages[1].clone());

        // Summary if present
        if let Some(summary) = &self.summary {
            messages.push(json!({
                "role": "system",
                "content": format!("## Previous Conversation Summary\nThe following is a summary of the earlier conversation. Use this to maintain context.\n\n{summary}")
            }));
        }

        // Buffer (recent messages)
        messages.extend(self.buffer.clone());
        messages
    }
}

mod mcp;
pub use mcp::{McpConnectionStatus, McpManager, McpRuntimeStateSnapshot, McpServerStatus};
mod tool;

use tool::{build_tools, execute_tool_call};

mod skills;
use skills::SkillsManager;

fn message_serialized_len(message: &Value) -> usize {
    message.to_string().len()
}

fn sum_message_lengths(messages: &[Value]) -> usize {
    messages.iter().map(message_serialized_len).sum()
}

pub fn load_openai_config(proxy: &mut dyn ShellProxy) -> OpenAiConfig {
    OpenAiConfig::from_getter(|key| proxy.get_var(key).or_else(|| std::env::var(key).ok()))
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
            let language = proxy.get_var(LANGUAGE_KEY);
            let model_override = model_override.map(|model| model.to_string());
            let mcp_manager = McpManager::load_blocking(proxy.list_mcp_servers());

            match chat_with_tools(
                &client,
                message,
                prompt,
                language,
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

#[allow(clippy::too_many_arguments)]
fn chat_with_tools(
    client: &ChatGptClient,
    user_input: &str,
    operator_prompt: Option<String>,
    language: Option<String>,
    temperature: Option<f64>,
    model_override: Option<String>,
    mcp_manager: &McpManager,
    proxy: &mut dyn ShellProxy,
) -> Result<String, String> {
    // Build System Prompt (fixed for the session)
    let system_prompt = json!({
        "role": "system",
        "content": build_system_prompt(operator_prompt, language, mcp_manager),
    });

    // First User Input (Pinned - the original goal)
    let first_user_message = json!({ "role": "user", "content": user_input });

    // Initialize Manager with pinned messages only
    let mut manager = ConversationManager::new(system_prompt, first_user_message);

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

        // Check for Summarization (Fix #5: may need multiple rounds if buffer is huge)
        while manager.should_summarize() {
            // Fix #4: Graceful fallback on summary failure
            if let Err(e) = manager.perform_summary(client, proxy, model_override.clone()) {
                tracing::warn!("Context summarization failed: {e}, continuing without summary");
                break; // Continue with current buffer, don't fail the whole conversation
            }
        }

        // Build fresh dynamic context each iteration (Fix #3)
        let dynamic_context = json!({
            "role": "user",
            "content": build_dynamic_context(proxy),
        });
        let current_messages = manager.build_messages_for_chat(dynamic_context);

        let response = {
            let spinner_text = "".to_string();

            let _spinner = SpinnerGuard::start(&spinner_text);
            client
                .send_chat_request(
                    &current_messages,
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

        // Add assistant response to history buffer
        manager.add_message(assistant_message.clone());

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

                // Add tool result to history buffer
                manager.add_message(json!({
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

fn build_system_prompt(
    operator_prompt: Option<String>,
    language: Option<String>,
    mcp_manager: &McpManager,
) -> String {
    let mut base = TOOL_SYSTEM_PROMPT.to_string();

    let skills_manager = SkillsManager::new();
    let skills_fragment = skills_manager.get_system_prompt_fragment();
    if !skills_fragment.is_empty() {
        base.push_str(&skills_fragment);
    }

    if let Some(fragment) = mcp_manager.system_prompt_fragment() {
        base.push_str("\n\nMCP access:");
        base.push('\n');
        base.push_str(&fragment);
    }

    if let Some(extra) = operator_prompt.and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }) {
        base.push_str("\n\nAdditional operator instructions:\n");
        base.push_str(&extra);
    }

    if let Some(lang) = language {
        let trimmed = lang.trim();
        if !trimmed.is_empty() {
            base.push_str("\n\nIMPORTANT: You MUST respond in ");
            base.push_str(trimmed);
            base.push('.');
        }
    }

    base
}

fn build_dynamic_context(proxy: &mut dyn ShellProxy) -> String {
    format!("Environment snapshot:\n{}", environment_snapshot(proxy))
}

fn environment_snapshot(proxy: &mut dyn ShellProxy) -> String {
    let os_family = std::env::consts::FAMILY;
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let current_dir = proxy
        .get_current_dir()
        .or_else(|_| std::env::current_dir())
        .ok();
    let cwd = current_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(failed to resolve current directory)".to_string());

    let alias_str = summarize_aliases(proxy.list_aliases().into_iter().collect());

    let mut files_str = String::new();
    if let Some(current_dir) = &current_dir
        && let Ok(entries) = fs::read_dir(current_dir)
    {
        let (visible_entries, total_entries) = collect_visible_entries_preview(entries);
        let summarized = summarize_visible_entries(visible_entries, total_entries);
        if !summarized.is_empty() {
            files_str = format!("\n- Visible files: {summarized}");
        }
    }

    format!(
        "- OS family: {os_family}\n- OS: {os}\n- Architecture: {arch}\n- Current directory: {cwd}\n- Git: {}{}- Aliases: {alias_str}",
        describe_git_state(),
        files_str
    )
}

fn summarize_aliases(mut aliases: Vec<(String, String)>) -> String {
    aliases.sort_by(|a, b| a.0.cmp(&b.0));
    let total = aliases.len();

    if total == 0 {
        return "none".to_string();
    }

    let mut rendered = aliases
        .into_iter()
        .take(MAX_VISIBLE_ALIASES)
        .map(|(name, cmd)| format!("{name}='{cmd}'"))
        .collect::<Vec<_>>();

    if total > MAX_VISIBLE_ALIASES {
        rendered.push(format!("(+{} more)", total - MAX_VISIBLE_ALIASES));
    }

    rendered.join(", ")
}

fn collect_visible_entries_preview(entries: fs::ReadDir) -> (Vec<(String, bool)>, usize) {
    let mut preview = Vec::new();
    let mut total = 0;

    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        total += 1;
        insert_visible_entry_preview(&mut preview, (name, is_dir), MAX_VISIBLE_FILES);
    }

    (preview, total)
}

fn insert_visible_entry_preview(
    preview: &mut Vec<(String, bool)>,
    entry: (String, bool),
    limit: usize,
) {
    let insert_at = preview
        .binary_search_by(|(current, _)| current.cmp(&entry.0))
        .unwrap_or_else(|idx| idx);

    if preview.len() < limit {
        preview.insert(insert_at, entry);
    } else if insert_at < limit {
        preview.insert(insert_at, entry);
        preview.truncate(limit);
    }
}

fn summarize_visible_entries(entries: Vec<(String, bool)>, total: usize) -> String {
    let shown = entries.len();

    if total == 0 {
        return String::new();
    }

    let mut rendered = entries
        .into_iter()
        .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name })
        .collect::<Vec<_>>();

    if total > shown {
        rendered.push(format!("(+{} more)", total - shown));
    }

    rendered.join(", ")
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

            match git_state_details() {
                Some((root, branch)) => match root {
                    Some(root) => format!("inside a Git worktree (root: {root}, {branch})"),
                    None => format!("inside a Git worktree ({branch})"),
                },
                None => "inside a Git worktree (branch unknown)".to_string(),
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

fn git_state_details() -> Option<(Option<String>, String)> {
    let output = Command::new("git")
        .args([
            "rev-parse",
            "--show-toplevel",
            "--abbrev-ref",
            "HEAD",
            "--short",
            "HEAD",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let root = lines.next().map(|line| line.to_string());
    let branch = lines.next()?;
    let short_head = lines.next().map(|line| line.to_string());

    let branch_description = if branch == "HEAD" {
        short_head
            .map(|commit| format!("detached at {commit}"))
            .unwrap_or_else(|| "detached HEAD".to_string())
    } else {
        format!("branch {branch}")
    };

    Some((root, branch_description))
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
        Value::String(text) if !text.is_empty() => {
            out.push(text.to_string());
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
fn summarize_visible_entries_for_test(mut entries: Vec<(String, bool)>) -> String {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let total = entries.len();
    summarize_visible_entries(entries.into_iter().take(MAX_VISIBLE_FILES).collect(), total)
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

    #[test]
    fn test_build_system_prompt_with_language() {
        let mcp_manager = McpManager::load_blocking(vec![]);

        // Case 1: No language
        let prompt_no_lang = build_system_prompt(None, None, &mcp_manager);
        assert!(!prompt_no_lang.contains("MUST respond in"));

        // Case 2: With language
        let prompt_lang = build_system_prompt(None, Some("Japanese".to_string()), &mcp_manager);
        assert!(prompt_lang.contains("IMPORTANT: You MUST respond in Japanese."));

        // Case 3: With language and operator prompt
        let prompt_mixed = build_system_prompt(
            Some("Be polite".to_string()),
            Some("French".to_string()),
            &mcp_manager,
        );
        assert!(prompt_mixed.contains("Additional operator instructions:\nBe polite"));
        assert!(prompt_mixed.contains("IMPORTANT: You MUST respond in French."));
    }

    #[test]
    fn system_prompt_uses_exact_tool_names() {
        assert!(TOOL_SYSTEM_PROMPT.contains("prefer `search` and `ls`"));
        assert!(TOOL_SYSTEM_PROMPT.contains("use `read_file` only after locating"));
        assert!(TOOL_SYSTEM_PROMPT.contains("- `read_file`: read a file"));
        assert!(!TOOL_SYSTEM_PROMPT.contains("- `read`:"));
    }

    #[test]
    fn summarize_aliases_limits_output() {
        let aliases = (0..10)
            .map(|idx| (format!("a{idx}"), format!("cmd{idx}")))
            .collect::<Vec<_>>();

        let summary = summarize_aliases(aliases);

        assert!(summary.contains("(+2 more)"));
        assert!(summary.contains("a0='cmd0'"));
    }

    #[test]
    fn summarize_visible_entries_limits_output() {
        let entries = (0..14)
            .map(|idx| (format!("file{idx}"), idx % 2 == 0))
            .collect::<Vec<_>>();

        let summary = summarize_visible_entries_for_test(entries);

        assert!(summary.contains("(+2 more)"));
        assert!(summary.contains("file0/"));
    }

    #[test]
    fn conversation_manager_tracks_buffer_size_incrementally() {
        let system_prompt = json!({"role": "system", "content": "sys"});
        let first_user_message = json!({"role": "user", "content": "hello"});
        let mut manager = ConversationManager::new(system_prompt, first_user_message);
        let msg1 = json!({"role": "assistant", "content": "abc"});
        let msg2 = json!({"role": "tool", "content": "def"});

        let expected = message_serialized_len(&msg1) + message_serialized_len(&msg2);
        manager.add_message(msg1);
        manager.add_message(msg2);

        assert_eq!(manager.buffer_size_chars(), expected);
    }
}
