use super::ShellProxy;
use crate::markdown::render_markdown_with_fallback;
use crate::markdown::stream::MarkdownBlockSplitter;
use crate::shell_capabilities::ChatToolHost;
use dsh_openai::turn::{self, TurnOutcome, extract_message_content, interpret_response};
use dsh_openai::{
    CANCELLED_MESSAGE, ChatGptClient, ChatRequestOptions, OpenAiConfig, is_ctrl_c_cancelled, usage,
};
use dsh_types::{Context, ExitStatus};
use indicatif::{ProgressBar, ProgressStyle};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

/// Environment variable key for storing the chat prompt template
const PROMPT_KEY: &str = "CHAT_PROMPT";
/// Primary configuration key for storing the default model
const MODEL_KEY: &str = "AI_CHAT_MODEL";
/// Environment variable key for storing the AI response language
const LANGUAGE_KEY: &str = "AI_MESSAGE_LANG";
/// Maximum number of iterations to satisfy tool calls before aborting.
/// Shared with the shell-side loop so the two cannot drift apart.
use dsh_openai::turn::limits::MAX_TOOL_ITERATIONS;
/// Threshold of characters in the buffer to trigger summarization (~3k-12k tokens)
const MAX_BUFFER_CHARS: usize = 96000;
/// Environment variable key to override the model used for summarization
const SUMMARY_MODEL_KEY: &str = "AI_SUMMARY_MODEL";
/// Environment key overriding the prompt-token ceiling before summarizing.
const CONTEXT_TOKEN_BUDGET_KEY: &str = "AI_CHAT_CONTEXT_TOKEN_BUDGET";
/// Default prompt-token ceiling before the conversation is summarized.
const DEFAULT_CONTEXT_TOKEN_BUDGET: u64 = 100_000;
/// Environment key capping what one `!` turn may spend, in total tokens.
const TURN_TOKEN_BUDGET_KEY: &str = "AI_CHAT_TURN_TOKEN_BUDGET";
/// Summarization attempts allowed before one turn gives up and proceeds.
const MAX_SUMMARY_ROUNDS: usize = 3;
/// Per-tool-result budget inside the text handed to the summarizer.
const MAX_SUMMARY_TOOL_CHARS: usize = 500;
/// Buffer *messages* kept verbatim by the deterministic compaction pass.
///
/// Counted in messages because that is what `retain_boundary` takes, and a
/// tool result always follows the assistant message that asked for it - so
/// this preserves roughly half as many results as its value suggests.
const RECENT_BUFFER_MESSAGES_KEPT: usize = 8;
/// A tool result this small is not worth a stub: the replacement text costs
/// about as much as the result did.
const MIN_ELIDABLE_TOOL_CHARS: usize = 400;
/// Cache-routing hint for providers that support it.
const PROMPT_CACHE_KEY: &str = "dsh-chat-agent";
/// Environment key toggling incremental Markdown rendering for `!` chat.
///
/// Streaming is opt-out, not opt-in: the escape hatch exists for a server
/// whose SSE support is broken in a way `send_chat_streaming`'s own
/// fallbacks do not catch, and for anyone who prefers the old
/// print-once-at-the-end behavior.
const STREAM_KEY: &str = "AI_CHAT_STREAM";

/// System prompt that explains how to use the builtin tools
const TOOL_SYSTEM_PROMPT: &str = r#"You are DogeShell Assistant, an autonomous software engineering agent running inside doge-shell.

Rules:
1. Briefly plan before using tools.
2. When the user refers to something that already happened - "the error just now", "why did that fail" - read it with `shell_history` instead of running the command again.
3. Explore cheaply first: prefer `search` and `ls`; use `read_file` only after locating the exact target.
4. Ask `shell_context` for the project's build and test commands rather than guessing them.
5. Verify every change. After editing, read the file back. After `execute`, check exit code, stdout, and stderr.
6. If a tool fails, analyze the error before asking the user.

Tools:
- `shell_history`: what the user recently ran, with exit codes and output
- `shell_context`: project root, runtimes, defined tasks, aliases
- `search`: find files or matching text
- `ls`: inspect directories
- `read_file`: read a line-numbered window of a file; it is paged, so continue with `offset`
- `str_replace`: change part of a file by exact match; use this for edits
- `edit`: create a file, or replace an existing one in full
- `execute`: run a shell command; pipes, redirection and `&&` all work

Respond in Markdown. Be concise and avoid unnecessary repetition.
"#;

struct ConversationManager {
    summary: Option<String>,
    buffer: Vec<Value>,
    buffer_chars: usize,
    /// Prompt tokens the provider reported for the most recent request.
    last_prompt_tokens: u64,
    /// Ceiling for that figure before the conversation is summarized.
    prompt_token_budget: u64,
    /// Usage billed to the current turn, accumulated locally.
    turn_usage: usage::TokenUsage,
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
            last_prompt_tokens: 0,
            prompt_token_budget: DEFAULT_CONTEXT_TOKEN_BUDGET,
            turn_usage: usage::TokenUsage::default(),
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

    /// Record what the provider actually charged for the last request.
    ///
    /// The byte length of the buffer is only a proxy; the reported prompt size
    /// also covers the system prompt, the tool schemas and the summary.
    fn note_prompt_tokens(&mut self, prompt_tokens: u64) {
        self.last_prompt_tokens = prompt_tokens;
    }

    fn set_prompt_token_budget(&mut self, budget: u64) {
        self.prompt_token_budget = budget;
    }

    /// Start a fresh usage tally for a new turn on a carried conversation.
    fn begin_turn(&mut self) {
        self.turn_usage = usage::TokenUsage::default();
    }

    fn should_summarize(&self) -> bool {
        self.buffer_size_chars() > MAX_BUFFER_CHARS
            || self.last_prompt_tokens > self.prompt_token_budget
    }

    /// Shrink the buffer without paying a model to do it.
    ///
    /// Summarizing costs a whole extra request, and most of what makes a long
    /// agent conversation large is not conversation at all: it is tool output
    /// the model has already acted on, and files it read more than once. Both
    /// can be dropped by rule.
    ///
    /// Only the `content` of a `tool` message is replaced, never the message
    /// itself. A `tool` message is only valid directly after the assistant
    /// message that asked for it, so removing one would leave the request
    /// dangling and the API answers that with a 400.
    ///
    /// Returns the number of characters reclaimed.
    fn compact_buffer(&mut self) -> usize {
        let before = self.buffer_chars;

        for index in self.superseded_tool_indices() {
            // The stub is not free. Replacing a two-byte "ok" with a sentence
            // naming the call makes the buffer *larger*, which is the opposite
            // of the job.
            if message_serialized_len(&self.buffer[index]) <= MIN_ELIDABLE_TOOL_CHARS {
                continue;
            }
            let label = tool_call_label(&self.buffer, index)
                .unwrap_or_else(|| "identical call".to_string());
            replace_tool_content(
                &mut self.buffer[index],
                &format!("(superseded by a later {label}; its newer result is below)"),
            );
        }

        // Everything before the last few exchanges is history the model has
        // already folded into what it did next.
        let keep_from = retain_boundary(&self.buffer, RECENT_BUFFER_MESSAGES_KEPT);
        for index in 0..keep_from {
            if message_role(&self.buffer[index]) != Some("tool") {
                continue;
            }
            let size = message_serialized_len(&self.buffer[index]);
            if size <= MIN_ELIDABLE_TOOL_CHARS {
                continue;
            }
            let label =
                tool_call_label(&self.buffer, index).unwrap_or_else(|| "tool result".to_string());
            replace_tool_content(
                &mut self.buffer[index],
                &format!("(elided: {label}, {size} bytes; call it again if you need it)"),
            );
        }

        self.buffer_chars = sum_message_lengths(&self.buffer);
        before.saturating_sub(self.buffer_chars)
    }

    /// Indices of tool results that a later identical call has replaced.
    ///
    /// Reading the same file twice used to keep both copies in the request for
    /// the rest of the conversation.
    fn superseded_tool_indices(&self) -> Vec<usize> {
        let mut latest: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut superseded = Vec::new();

        for index in 0..self.buffer.len() {
            let Some(key) = tool_call_signature(&self.buffer, index) else {
                continue;
            };
            if let Some(previous) = latest.insert(key, index) {
                superseded.push(previous);
            }
        }

        superseded
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
                let mut content = extract_message_content(msg).unwrap_or_default();
                if role == "tool" && content.len() > MAX_SUMMARY_TOOL_CHARS {
                    // The summary needs the gist, not the whole build log.
                    let end = content.floor_char_boundary(MAX_SUMMARY_TOOL_CHARS);
                    content = format!("{}... (truncated)", &content[..end]);
                }

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
        // No `max_completion_tokens`: on a reasoning model that budget also
        // covers hidden reasoning, so a tight cap returns finish_reason=length
        // with no summary at all.
        let options = ChatRequestOptions::new()
            .with_temperature(Some(0.3)) // Lower temperature for consistent summarization
            .with_model(summary_model);
        let response = client
            .send_chat(&summary_messages, &options, Some(&|| proxy.is_canceled()))
            .map_err(|e| format!("Summarization failed: {e}"))?;
        self.turn_usage.add_response(&response);

        let new_summary = summary_from_response(&response)?;

        // Update state: keep most recent messages to maintain tool_call/result continuity
        const RETAIN_AFTER_SUMMARY: usize = 6; // Keep last ~3 exchanges (assistant+tool pairs)
        let retain_start = retain_boundary(&self.buffer, RETAIN_AFTER_SUMMARY);
        self.buffer = self.buffer.split_off(retain_start);
        self.buffer_chars = sum_message_lengths(&self.buffer);
        self.summary = Some(new_summary);
        // The measured prompt size describes the request we just replaced. Left
        // in place it keeps `should_summarize` true, and the caller's
        // `while` loop bills a summarization request per iteration forever.
        self.last_prompt_tokens = 0;

        Ok(())
    }

    /// Assemble the request, stable prefix first.
    ///
    /// Providers cache the longest common prefix of a request, so nothing
    /// volatile may appear before the conversation. The environment snapshot
    /// used to sit at index 1, which invalidated the cache for the whole
    /// conversation every time a file or the git branch changed.
    fn build_messages_for_chat(&self, dynamic_context: Value) -> Vec<Value> {
        let mut messages = Vec::new();

        // System prompt (index 0)
        messages.push(self.pinned_messages[0].clone());

        // First user message (index 1, pinned) - the original goal
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

        // Volatile environment snapshot last.
        messages.push(dynamic_context);
        messages
    }
}

mod mcp;
pub use mcp::{McpConnectionStatus, McpManager, McpRuntimeStateSnapshot, McpServerStatus};
mod tool;

use tool::{build_tools, execute_tool_call};

mod session;

mod skills;
use skills::SkillsManager;

/// Where to cut the buffer so that `retain` messages survive a summary.
///
/// A `tool` message is only valid immediately after the assistant message that
/// requested it. Cutting between the two leaves an orphan that the API rejects
/// with a 400, which used to surface as a failure right after every summary of
/// a long session. Walking backwards keeps at most one extra exchange.
fn retain_boundary(buffer: &[Value], retain: usize) -> usize {
    let mut start = buffer.len().saturating_sub(retain);
    while start > 0 && message_role(&buffer[start]) == Some("tool") {
        start -= 1;
    }
    start
}

/// The call a `tool` message answers: its function name and its arguments.
///
/// Derived from the assistant message that requested it rather than stored on
/// the tool message, because everything on that message is sent to the
/// provider and an unknown field is something an endpoint may reject.
fn tool_call_target(buffer: &[Value], index: usize) -> Option<(String, String)> {
    let message = buffer.get(index)?;
    if message_role(message)? != "tool" {
        return None;
    }
    let call_id = message.get("tool_call_id").and_then(Value::as_str)?;

    // The request sits in the nearest preceding assistant message.
    buffer[..index].iter().rev().find_map(|candidate| {
        let calls = candidate.get("tool_calls")?.as_array()?;
        let call = calls
            .iter()
            .find(|call| call.get("id").and_then(Value::as_str) == Some(call_id))?;
        let function = call.get("function")?;
        let name = function.get("name")?.as_str()?.to_string();
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_string();
        Some((name, arguments))
    })
}

/// Two calls with the same name and arguments return the same thing, so only
/// the newer one is worth carrying.
fn tool_call_signature(buffer: &[Value], index: usize) -> Option<String> {
    tool_call_target(buffer, index).map(|(name, arguments)| format!("{name}({arguments})"))
}

/// A short label for the stub left where a result used to be.
///
/// The stub has to name the call, or the model cannot judge whether re-running
/// it is worth a turn.
fn tool_call_label(buffer: &[Value], index: usize) -> Option<String> {
    let (name, arguments) = tool_call_target(buffer, index)?;
    Some(format!("{name}({})", shorten_arguments(&arguments)))
}

/// Enough of the arguments to identify the call, and no more.
fn shorten_arguments(arguments: &str) -> String {
    const MAX: usize = 80;
    let trimmed = arguments.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let end = trimmed.floor_char_boundary(MAX);
    format!("{}...", &trimmed[..end])
}

/// Swap a tool result's content for a stub, leaving the message in place.
fn replace_tool_content(message: &mut Value, stub: &str) {
    if let Some(map) = message.as_object_mut() {
        map.insert("content".into(), Value::String(stub.to_string()));
    }
}

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(|role| role.as_str())
}

/// Read a setting shell-variable first, then the process environment.
///
/// `proxy.set_var` (and `(vset ...)`) writes into the shell `Environment`, not
/// the process env, so an env-only lookup silently ignores it.
fn resolve_setting(proxy: &mut dyn ShellProxy, key: &str) -> Option<String> {
    proxy
        .get_var(key)
        .or_else(|| std::env::var(key).ok())
        .filter(|value| !value.trim().is_empty())
}

/// Whether `!` chat should stream its answer as it is generated.
///
/// Default on: `0` / `false` / `off` / `no` (case-insensitive) opt out.
fn resolve_stream_enabled(proxy: &mut dyn ShellProxy) -> bool {
    match resolve_setting(proxy, STREAM_KEY) {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// Ceiling on what one turn may spend, or `None` when unset.
///
/// `MAX_TOOL_ITERATIONS` bounds the number of steps, not their cost: a hundred
/// iterations over a large context is a bill, not a guard rail. Off by default,
/// because the right number depends on the model and on what the user is
/// willing to spend.
fn resolve_turn_token_budget(proxy: &mut dyn ShellProxy) -> Option<u64> {
    resolve_setting(proxy, TURN_TOKEN_BUDGET_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|budget| *budget > 0)
}

/// Prompt-token ceiling that forces a summary regardless of buffer bytes.
fn resolve_prompt_token_budget(proxy: &mut dyn ShellProxy) -> u64 {
    resolve_setting(proxy, CONTEXT_TOKEN_BUDGET_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|budget| *budget > 0)
        .unwrap_or(DEFAULT_CONTEXT_TOKEN_BUDGET)
}

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
    proxy: &mut dyn ChatToolHost,
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
            // The shell's own manager, so `mcp connect` / `mcp disconnect` /
            // `mcp status` and the agent describe the same connections.
            let mcp_manager = proxy.agent_mcp_manager();

            let stream_enabled = resolve_stream_enabled(proxy);
            let mut sink = stream_enabled.then(|| StreamSink::new(ctx));

            match chat_with_tools(
                &client,
                message,
                prompt,
                language,
                Some(0.1),
                model_override,
                &mcp_manager,
                sink.as_mut(),
                proxy,
            ) {
                Ok(res) => {
                    // Already on the screen: streaming rendered this same
                    // text (the final iteration's content, unchanged) block
                    // by block as it arrived. This must be the *last*
                    // iteration's own flag, not "did any earlier iteration
                    // stream something" - a per-request fallback can leave
                    // an interim round streamed but the round that produced
                    // `res` un-streamed, and `wrote_any` alone would then
                    // skip printing the answer entirely.
                    let already_shown = sink
                        .as_ref()
                        .is_some_and(StreamSink::streamed_this_iteration);
                    if !already_shown {
                        let rendered = render_markdown_with_fallback(res.trim());
                        let trimmed = rendered.trim_end_matches('\n');
                        ctx.write_stdout(trimmed).ok();
                    }
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

/// Built-in chat_reset command description
pub fn chat_reset_description() -> &'static str {
    "Forget the carried AI chat conversation"
}

/// Built-in chat_reset command implementation
///
/// Consecutive `!` turns continue the same conversation; this starts over.
pub fn chat_reset(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() > 1 {
        ctx.write_stderr("Usage: chat_reset").ok();
        return ExitStatus::ExitedWith(1);
    }

    let detail = session::session_description();
    let cleared = session::session_reset();

    let message = match (cleared, detail) {
        (true, Some(detail)) => format!("chat session cleared ({detail})"),
        (true, None) => "chat session cleared".to_string(),
        (false, _) => "no chat session to clear".to_string(),
    };
    ctx.write_stdout(&message).ok();
    ExitStatus::ExitedWith(0)
}

/// Describe the carried conversation, for `doctor ai`.
pub fn chat_session_description() -> Option<String> {
    session::session_description()
}

#[allow(clippy::too_many_arguments)]
fn chat_with_tools(
    client: &ChatGptClient,
    user_input: &str,
    operator_prompt: Option<String>,
    language: Option<String>,
    temperature: Option<f64>,
    model_override: Option<String>,
    mcp_manager: &Arc<RwLock<McpManager>>,
    mut stream_sink: Option<&mut StreamSink>,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    // Build System Prompt (fixed for the session)
    let system_prompt_text = build_system_prompt(operator_prompt, language, &mcp_manager.read());
    let cwd = proxy.get_current_dir().ok();

    let session_ttl = session::resolve_ttl(resolve_setting(proxy, session::SESSION_TTL_KEY));

    // Continue the previous conversation when it still applies, so a follow-up
    // question does not re-explore the repository from scratch.
    let mut manager = match session::take(session_ttl, &system_prompt_text, cwd.as_deref()) {
        Some(mut manager) => {
            manager.add_message(json!({ "role": "user", "content": user_input }));
            manager
        }
        None => ConversationManager::new(
            json!({ "role": "system", "content": system_prompt_text.clone() }),
            // First User Input (Pinned - the original goal)
            json!({ "role": "user", "content": user_input }),
        ),
    };
    manager.set_prompt_token_budget(resolve_prompt_token_budget(proxy));
    manager.begin_turn();
    let turn_token_budget = resolve_turn_token_budget(proxy);

    let mut tools = build_tools();
    {
        let mcp = mcp_manager.read();
        if !mcp.is_empty() {
            tools.extend(mcp.tool_definitions());
        }
    }
    let mut iterations = 0;
    let mut dynamic_context = DynamicContext::default();
    // Rounds where the model produced neither a tool call nor an answer.
    let mut stalled_rounds = 0usize;

    let outcome = loop {
        iterations += 1;
        if iterations > MAX_TOOL_ITERATIONS {
            break Err("chat: exceeded maximum number of tool interactions".to_string());
        }

        // Checked before the request, not after: stopping once the bill is
        // already over the line would let a single expensive turn blow through
        // whatever number the user set.
        if let Some(budget) = turn_token_budget
            && manager.turn_usage.total_tokens() >= budget
        {
            break Err(format!(
                "chat: stopped after {} tokens, at the {TURN_TOKEN_BUDGET_KEY} of {budget}. \
                 Raise it, or ask a narrower question.",
                manager.turn_usage.total_tokens()
            ));
        }

        // Compact by rule before paying a model to summarize. Superseded and
        // stale tool output is most of what makes a long run large, and
        // dropping it costs nothing; on the runs where this is enough, the
        // summarization request below never happens.
        if manager.should_summarize() {
            let reclaimed = manager.compact_buffer();
            if reclaimed > 0 {
                tracing::debug!("compacted {reclaimed} chars of tool output out of the buffer");
            }
        }

        // Check for Summarization (may need multiple rounds if buffer is huge).
        // Bounded: a summary that fails to shrink the context must not turn into
        // an unbounded stream of paid requests.
        let mut summary_rounds = 0;
        while manager.should_summarize() && summary_rounds < MAX_SUMMARY_ROUNDS {
            summary_rounds += 1;
            // Graceful fallback on summary failure
            if let Err(e) = manager.perform_summary(client, proxy, model_override.clone()) {
                tracing::warn!("Context summarization failed: {e}, continuing without summary");
                break; // Continue with current buffer, don't fail the whole conversation
            }
        }

        let current_messages = manager.build_messages_for_chat(dynamic_context.message(proxy));

        let options = ChatRequestOptions::new()
            .with_temperature(temperature)
            .with_model(model_override.clone())
            .with_tools(Some(tools.clone()))
            .with_prompt_cache_key(Some(PROMPT_CACHE_KEY.to_string()))
            .with_stream(stream_sink.is_some());

        let response = if let Some(sink) = stream_sink.as_deref_mut() {
            sink.begin_iteration();
            let spinner = SpinnerGuard::start("");
            let result = client.send_chat_streaming(
                &current_messages,
                &options,
                Some(&|| proxy.is_canceled()),
                &mut |text| sink.on_delta(&spinner, text),
            );
            match result {
                Ok(response) => {
                    sink.finish_iteration(&spinner);
                    response
                }
                Err(err) => {
                    // Whatever streamed before the failure (a dropped
                    // connection, a mid-stream error frame) is already
                    // generated - flush it instead of losing the tail end
                    // of a partial answer the user never gets to see.
                    sink.finish_iteration(&spinner);
                    break Err(if is_ctrl_c_cancelled(&err) {
                        err.to_string()
                    } else {
                        format!("chat: {err}")
                    });
                }
            }
        } else {
            let _spinner = SpinnerGuard::start("");
            match client.send_chat(&current_messages, &options, Some(&|| proxy.is_canceled())) {
                Ok(response) => response,
                Err(err) => {
                    break Err(if is_ctrl_c_cancelled(&err) {
                        err.to_string()
                    } else {
                        format!("chat: {err}")
                    });
                }
            }
        };

        // Feed the measured prompt size back so the next summarization
        // decision is based on what the provider charged, not a byte proxy.
        manager.turn_usage.add_response(&response);
        if let Some(reported) = usage::TokenUsage::from_response(&response) {
            manager.note_prompt_tokens(reported.prompt_tokens);
        }

        let turn = match interpret_response(&response) {
            Ok(turn) => turn,
            Err(err) => break Err(format!("chat: {err}")),
        };

        // Streamed this round's text already appeared as rendered Markdown
        // blocks; a response that fell back to non-streaming (or streaming
        // is off) still owes the old dim interim-text line.
        let streamed_this_round = stream_sink
            .as_deref()
            .is_some_and(StreamSink::streamed_this_iteration);

        // A run of up to MAX_TOOL_ITERATIONS steps is otherwise a black box:
        // show the plan the model states alongside its tool calls.
        if !streamed_this_round && let Some(text) = &turn.interim_text {
            eprintln!("\x1b[2m{}\x1b[0m", text.trim());
        }

        if let Some(assistant_message) = turn.assistant_message {
            manager.add_message(assistant_message);
        }

        match turn.outcome {
            TurnOutcome::ToolCalls(tool_calls) => {
                stalled_rounds = 0;

                for tool_call in &tool_calls {
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
            }
            TurnOutcome::Answer(content) => break Ok(content),
            TurnOutcome::Cut {
                finish_reason,
                partial,
            } => {
                // Already on the screen for this round; showing it again in
                // the error would duplicate it.
                let partial = if streamed_this_round {
                    None
                } else {
                    partial.as_deref()
                };
                break Err(format!(
                    "chat: {}",
                    turn::describe_cut(&finish_reason, partial)
                ));
            }
            TurnOutcome::Stalled => {
                // Nudge once, then stop instead of resending the same request
                // until the iteration cap burns through the budget.
                stalled_rounds += 1;
                match turn::handle_stall(stalled_rounds) {
                    turn::StallAction::GiveUp(reason) => break Err(format!("chat: {reason}")),
                    turn::StallAction::Nudge(prompt) => {
                        manager.add_message(json!({ "role": "user", "content": prompt }));
                    }
                }
            }
        }
    };

    report_turn_usage(&manager.turn_usage);

    // Only a completed turn is worth resuming. Carrying a cancelled or failed
    // one forward would replay its dead end - including the synthetic nudge -
    // as the starting context of the next question.
    if outcome.is_ok() {
        session::store(session_ttl, manager, &system_prompt_text, cwd);
    }

    outcome
}

/// Print what this turn cost, so context changes can be judged.
fn report_turn_usage(turn: &usage::TokenUsage) {
    if turn.is_empty() {
        return;
    }
    eprintln!("\x1b[2mtokens: {}\x1b[0m", turn.summary_line());
}

struct SpinnerGuard {
    progress: ProgressBar,
}

impl SpinnerGuard {
    fn start(message: &str) -> Self {
        let progress = ProgressBar::new_spinner();
        // `wide_msg` (rather than `msg`) elides the message to fit the
        // remaining terminal width, so a long in-progress preview
        // (`set_tail`) cannot wrap the spinner onto a second line.
        let style = ProgressStyle::with_template("{spinner} {wide_msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_chars("-\\|/");
        progress.set_style(style);
        progress.set_message(message.to_string());
        progress.enable_steady_tick(Duration::from_millis(80));
        SpinnerGuard { progress }
    }

    /// Hide the spinner line, run `f`, then let it resume drawing.
    ///
    /// `indicatif` owns the bottom line while ticking; writing to stdout
    /// during that window without this would land the write in the middle
    /// of the spinner's own redraw.
    fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        self.progress.suspend(f)
    }

    /// Show a single-line preview of text still generating.
    fn set_tail(&self, text: &str) {
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.progress.set_message(collapsed);
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.progress.finish_and_clear();
    }
}

/// Streams one `!` chat turn's answer to the terminal as it arrives, instead
/// of waiting for the whole turn to finish.
///
/// Confirmed Markdown blocks ([`MarkdownBlockSplitter`]) are rendered and
/// written the moment they are safe to render (see that type's docs for why
/// that moment is safe); the unconfirmed remainder is shown as a raw preview
/// on the spinner's status line. This mirrors what `execute_chat_message`
/// already does for a complete answer - `render_markdown_with_fallback`
/// then `ctx.write_stdout` - just spread across many smaller calls instead
/// of one, so the total bytes written for a turn are the same either way.
struct StreamSink<'a> {
    ctx: &'a Context,
    splitter: MarkdownBlockSplitter,
    /// Set once anything has been written this turn (any iteration).
    wrote_any: bool,
    /// Reset at the start of each iteration by [`Self::begin_iteration`];
    /// tells the caller whether *this* iteration's response streamed
    /// anything, since a per-round fallback to a non-streaming response can
    /// happen even when the sink itself is active for the whole turn.
    streamed_this_iteration: bool,
}

impl<'a> StreamSink<'a> {
    fn new(ctx: &'a Context) -> Self {
        Self {
            ctx,
            splitter: MarkdownBlockSplitter::new(),
            wrote_any: false,
            streamed_this_iteration: false,
        }
    }

    fn streamed_this_iteration(&self) -> bool {
        self.streamed_this_iteration
    }

    /// Call once before each `send_chat_streaming` attempt.
    fn begin_iteration(&mut self) {
        self.streamed_this_iteration = false;
    }

    /// Feed one text delta, writing any block it completes.
    fn on_delta(&mut self, spinner: &SpinnerGuard, text: &str) {
        if !text.is_empty() {
            self.streamed_this_iteration = true;
        }
        for block in self.splitter.push(text) {
            self.write_block(spinner, &block);
        }
        spinner.set_tail(&self.splitter.pending_tail());
    }

    /// Flush whatever is left at the end of one iteration's response, and
    /// reset for the next - a tool-call round and the answer that follows it
    /// are separate Markdown documents, and an open list or fence from one
    /// must not bleed into the other.
    fn finish_iteration(&mut self, spinner: &SpinnerGuard) {
        for block in self.splitter.finish() {
            self.write_block(spinner, &block);
        }
        spinner.set_tail("");
    }

    fn write_block(&mut self, spinner: &SpinnerGuard, block: &str) {
        let rendered = render_markdown_with_fallback(block.trim());
        if rendered.trim().is_empty() {
            return;
        }
        // `Context::write_stdout` always appends exactly one `\n`, so
        // prefixing every block but the first with one more reproduces the
        // single blank line `TerminalRenderer` puts between any two
        // top-level blocks when rendering the whole answer at once.
        let text = if self.wrote_any {
            format!("\n{rendered}")
        } else {
            rendered
        };
        let ctx = self.ctx;
        spinner.suspend(|| {
            ctx.write_stdout(&text).ok();
        });
        self.wrote_any = true;
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

    dsh_openai::apply_language(&base, language.as_deref())
}

/// The operator's response language, for any AI request the shell makes.
///
/// Public because `ai-commit`, `safe-run` and `blocks` need the same answer:
/// `AI_MESSAGE_LANG` used to reach the `!` runtime and nothing else.
pub fn response_language(proxy: &mut dyn ShellProxy) -> Option<String> {
    proxy
        .get_var(LANGUAGE_KEY)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Environment snapshot for one agent run, rebuilt only when it changes.
///
/// It used to be regenerated on every iteration, which cost two `git`
/// subprocesses per tool call for information that rarely moves.
#[derive(Default)]
struct DynamicContext {
    signature: Option<EnvironmentSignature>,
    rendered: String,
}

/// What the snapshot depends on.
///
/// Only the directory and the repository state now: the file list and the
/// alias table moved out of the snapshot, and with them the reasons to watch
/// the directory mtime and count aliases on every iteration.
#[derive(PartialEq, Eq)]
struct EnvironmentSignature {
    cwd: PathBuf,
    git_head_modified_ms: u128,
}

impl DynamicContext {
    fn message(&mut self, proxy: &mut dyn ShellProxy) -> Value {
        let signature = environment_signature(proxy);
        if self.signature.as_ref() != Some(&signature) {
            self.rendered = build_dynamic_context(proxy);
            self.signature = Some(signature);
        }

        json!({ "role": "user", "content": self.rendered.clone() })
    }
}

fn environment_signature(proxy: &mut dyn ShellProxy) -> EnvironmentSignature {
    let cwd = proxy
        .get_current_dir()
        .or_else(|_| std::env::current_dir())
        .unwrap_or_default();

    EnvironmentSignature {
        git_head_modified_ms: git_head_path(&cwd)
            .map(|head| modified_ms(&head))
            .unwrap_or(0),
        cwd,
    }
}

fn modified_ms(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

/// Find the `HEAD` of the repository containing `start` without spawning `git`.
///
/// Checkouts rewrite this file; commits that only move a ref do not, so the
/// directory mtime is what catches ordinary edits.
fn git_head_path(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let git = dir.join(".git");
        if git.is_dir() {
            return Some(git.join("HEAD"));
        }
        if git.is_file() {
            // Linked worktree or submodule: the pointer file itself changes.
            return Some(git);
        }
        current = dir.parent();
    }
    None
}

/// The summary text, or why there is none.
///
/// Through `turn`, like every other answer. Reading `choices[0].message` by
/// hand accepted a summary the provider had cut short
/// (`finish_reason=length`), and because the caller drops the buffer this
/// summary replaces, that loss is not recoverable. Returning `Err` leaves the
/// conversation as it was and only skips the compaction.
fn summary_from_response(response: &Value) -> Result<String, String> {
    turn::answer_text(response)
        .map_err(|err| format!("Summarization returned no usable summary: {err}"))
}

fn build_dynamic_context(proxy: &mut dyn ShellProxy) -> String {
    format!(
        "Environment snapshot (reference only; the task is stated in the first user message):\n{}",
        environment_snapshot(proxy)
    )
}

/// The few facts worth paying for on every single request.
///
/// The file list and the alias table used to live here too. Both are answers to
/// questions the model asks occasionally, and both were being re-sent on every
/// iteration of a hundred-step run; `ls` and `shell_context` now serve them on
/// demand instead.
fn environment_snapshot(proxy: &mut dyn ShellProxy) -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let cwd = proxy
        .get_current_dir()
        .or_else(|_| std::env::current_dir())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "(failed to resolve current directory)".to_string());

    format!(
        "- OS: {os} ({arch})\n- Current directory: {cwd}\n- Git: {}",
        describe_git_state()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant_call(id: &str, name: &str, arguments: &str) -> Value {
        json!({
            "role": "assistant",
            "tool_calls": [{
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": arguments }
            }]
        })
    }

    fn tool_reply(id: &str, content: &str) -> Value {
        json!({ "role": "tool", "tool_call_id": id, "content": content })
    }

    fn manager_with(buffer: Vec<Value>) -> ConversationManager {
        let mut manager = ConversationManager::new(
            json!({ "role": "system", "content": "sys" }),
            json!({ "role": "user", "content": "goal" }),
        );
        for message in buffer {
            manager.add_message(message);
        }
        manager
    }

    /// Reading the same file twice used to keep both copies in every later
    /// request for the rest of the conversation.
    #[test]
    fn compaction_drops_a_result_a_later_identical_call_replaced() {
        let payload = "x".repeat(2000);
        let mut manager = manager_with(vec![
            assistant_call("a", "read_file", r#"{"path":"src/main.rs"}"#),
            tool_reply("a", &payload),
            assistant_call("b", "read_file", r#"{"path":"src/main.rs"}"#),
            tool_reply("b", &payload),
        ]);

        let reclaimed = manager.compact_buffer();

        assert!(reclaimed > 1500, "reclaimed {reclaimed}");
        let first = extract_message_content(&manager.buffer[1]).unwrap();
        assert!(first.contains("superseded"), "{first}");
        assert!(first.contains("read_file"), "{first}");
        // The newer copy is untouched.
        assert_eq!(
            extract_message_content(&manager.buffer[3]).unwrap(),
            payload
        );
    }

    /// Old tool output becomes a stub that still names the call, so the model
    /// can decide whether fetching it again is worth a turn.
    #[test]
    fn compaction_elides_stale_output_but_says_what_it_was() {
        let mut buffer = Vec::new();
        for index in 0..8 {
            let id = format!("c{index}");
            buffer.push(assistant_call(
                &id,
                "search",
                &format!(r#"{{"query":"q{index}"}}"#),
            ));
            buffer.push(tool_reply(&id, &"y".repeat(2000)));
        }
        let mut manager = manager_with(buffer);

        manager.compact_buffer();

        let oldest = extract_message_content(&manager.buffer[1]).unwrap();
        assert!(oldest.contains("elided"), "{oldest}");
        assert!(oldest.contains("search("), "{oldest}");

        let newest = extract_message_content(manager.buffer.last().unwrap()).unwrap();
        assert_eq!(newest.len(), 2000, "the recent window must survive intact");
    }

    /// Every tool message has to keep its place, or the API rejects the request:
    /// a `tool` message is only valid right after the call that asked for it.
    #[test]
    fn compaction_never_removes_a_message() {
        let mut buffer = Vec::new();
        for index in 0..8 {
            let id = format!("c{index}");
            buffer.push(assistant_call(&id, "ls", r#"{"path":"."}"#));
            buffer.push(tool_reply(&id, &"z".repeat(2000)));
        }
        let mut manager = manager_with(buffer);
        let before = manager.buffer.len();

        manager.compact_buffer();

        assert_eq!(manager.buffer.len(), before);
        for (index, message) in manager.buffer.iter().enumerate() {
            let expected = if index % 2 == 0 { "assistant" } else { "tool" };
            assert_eq!(message_role(message), Some(expected));
        }
    }

    /// A stub costs about as much as a short result, so short results stay.
    #[test]
    fn compaction_leaves_small_results_alone() {
        let mut buffer = Vec::new();
        for index in 0..8 {
            let id = format!("c{index}");
            buffer.push(assistant_call(&id, "ls", r#"{"path":"."}"#));
            buffer.push(tool_reply(&id, "ok"));
        }
        let mut manager = manager_with(buffer);

        assert_eq!(manager.compact_buffer(), 0);
        assert_eq!(extract_message_content(&manager.buffer[1]).unwrap(), "ok");
    }

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
    fn a_truncated_summary_is_not_accepted_as_one() {
        let cut = json!({
            "choices": [{
                "message": {"role": "assistant", "content": "The user asked about the pa"},
                "finish_reason": "length"
            }]
        });

        let err = summary_from_response(&cut).expect_err("a cut summary is not a summary");
        assert!(err.contains("cut off"), "{err}");

        let complete = json!({
            "choices": [{
                "message": {"role": "assistant", "content": "The user asked about the parser."},
                "finish_reason": "stop"
            }]
        });
        assert_eq!(
            summary_from_response(&complete).unwrap(),
            "The user asked about the parser."
        );
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
        assert!(TOOL_SYSTEM_PROMPT.contains("- `read_file`: read a line-numbered window"));
        assert!(TOOL_SYSTEM_PROMPT.contains("- `str_replace`:"));
        assert!(!TOOL_SYSTEM_PROMPT.contains("- `read`:"));
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
    fn assistant_with_tool_calls(id: &str) -> Value {
        json!({
            "role": "assistant",
            "tool_calls": [{"id": id, "function": {"name": "ls", "arguments": "{}"}}]
        })
    }

    fn tool_result(id: &str) -> Value {
        json!({"role": "tool", "tool_call_id": id, "content": "ok"})
    }

    #[test]
    fn retain_boundary_does_not_orphan_tool_messages() {
        // assistant(3 calls) + 3 tool results, twice over.
        let buffer = vec![
            json!({"role": "assistant", "content": "plan"}),
            assistant_with_tool_calls("a"),
            tool_result("a1"),
            tool_result("a2"),
            tool_result("a3"),
            assistant_with_tool_calls("b"),
            tool_result("b1"),
            tool_result("b2"),
        ];

        // A naive `len - 6` would start at index 2, which is a tool message.
        let start = retain_boundary(&buffer, 6);

        assert_eq!(start, 1);
        assert_eq!(message_role(&buffer[start]), Some("assistant"));
    }

    #[test]
    fn retain_boundary_keeps_an_already_valid_cut() {
        let buffer = vec![
            json!({"role": "assistant", "content": "one"}),
            json!({"role": "assistant", "content": "two"}),
            json!({"role": "assistant", "content": "three"}),
        ];

        assert_eq!(retain_boundary(&buffer, 2), 1);
        assert_eq!(retain_boundary(&buffer, 99), 0);
    }

    #[test]
    fn retain_boundary_stops_at_zero_for_a_buffer_of_tool_results() {
        let buffer = vec![tool_result("a"), tool_result("b")];
        assert_eq!(retain_boundary(&buffer, 1), 0);
    }

    #[test]
    fn build_messages_for_chat_puts_the_volatile_snapshot_last() {
        // Anything before the conversation invalidates the provider's prefix
        // cache whenever the working tree moves.
        let mut manager = ConversationManager::new(
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "goal"}),
        );
        manager.summary = Some("earlier work".to_string());
        manager.add_message(json!({"role": "assistant", "content": "step"}));

        let snapshot = json!({"role": "user", "content": "Environment snapshot: ..."});
        let messages = manager.build_messages_for_chat(snapshot.clone());

        assert_eq!(messages[0]["content"], "sys");
        assert_eq!(messages[1]["content"], "goal");
        assert_eq!(messages[2]["role"], "system");
        assert!(
            messages[2]["content"]
                .as_str()
                .unwrap()
                .contains("earlier work")
        );
        assert_eq!(messages[3]["content"], "step");
        assert_eq!(messages.last().unwrap(), &snapshot);
    }

    #[test]
    fn should_summarize_reacts_to_measured_prompt_tokens() {
        let mut manager = ConversationManager::new(
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "goal"}),
        );

        assert!(!manager.should_summarize());

        manager.note_prompt_tokens(DEFAULT_CONTEXT_TOKEN_BUDGET + 1);
        assert!(manager.should_summarize());

        // A summary must clear the condition, or the caller's `while` loop
        // bills a summarization request per iteration forever.
        manager.last_prompt_tokens = 0;
        assert!(!manager.should_summarize());
    }
}
