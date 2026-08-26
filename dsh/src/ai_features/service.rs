//! AI Service traits and implementations.
//!
//! This module defines the core AI service abstraction and the live implementation
//! that integrates with OpenAI API and MCP tools.

use crate::repl::confirmation::ConfirmationAction;
use crate::safety::{SafetyGuard, SafetyLevel, SafetyResult};
use anyhow::Result;
use async_trait::async_trait;
use dsh_builtin::McpManager;
use dsh_openai::turn::{self, TurnOutcome};
use dsh_openai::{ChatGptClient, ChatRequestOptions};
use parking_lot::RwLock;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Shape of one AI request beyond the messages themselves.
#[derive(Debug, Clone, Default)]
pub struct AiRequestOptions {
    pub temperature: Option<f64>,
    /// Cap on generated tokens.
    ///
    /// Beware: on a reasoning model this budget also covers hidden reasoning, so
    /// a tight cap returns nothing. See `ChatRequestOptions::max_tokens`.
    pub max_tokens: Option<u64>,
    /// Ask the provider to guarantee a JSON object, instead of hoping the
    /// prompt is obeyed and then stripping code fences.
    pub json_object: bool,
}

impl AiRequestOptions {
    pub fn new(temperature: Option<f64>) -> Self {
        Self {
            temperature,
            ..Self::default()
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: Option<u64>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn as_json_object(mut self) -> Self {
        self.json_object = true;
        self
    }

    fn to_chat_options(&self, tools: Option<Vec<Value>>) -> ChatRequestOptions {
        let mut options = ChatRequestOptions::new()
            .with_temperature(self.temperature)
            .with_tools(tools)
            .with_max_tokens(self.max_tokens);
        if self.json_object {
            options = options.with_response_format(Some(dsh_openai::json_object_format()));
        }
        options
    }
}

/// Cap for a single MCP tool result fed back into the conversation.
const MAX_TOOL_OUTPUT_CHARS: usize = 4096;

/// Response structure for AI-generated commands
#[derive(Debug, Deserialize)]
pub struct AiCommandResponse {
    pub command: String,
    pub args: Vec<String>,
}

/// Core AI service trait for sending requests to AI backends.
#[async_trait]
pub trait AiService: Send + Sync {
    /// Send a request to the AI service with the given messages and temperature.
    async fn send_request(&self, messages: Vec<Value>, temperature: Option<f64>) -> Result<String>;

    /// Same as [`AiService::send_request`], with request options.
    ///
    /// Callers that are going to parse the answer ask for JSON here rather than
    /// hoping the prompt is obeyed and then stripping code fences.
    async fn send_request_with(
        &self,
        messages: Vec<Value>,
        options: AiRequestOptions,
    ) -> Result<String> {
        self.send_request(messages, options.temperature).await
    }

    /// Get the safety guard if available.
    fn get_safety_guard(&self) -> Option<Arc<SafetyGuard>> {
        None
    }

    /// Get the current safety level.
    fn get_safety_level(&self) -> Option<SafetyLevel> {
        None
    }

    /// Get the command allowlist.
    fn get_allowlist(&self) -> Option<Vec<String>> {
        None
    }
}

/// Handler for user confirmations.
#[async_trait]
pub trait ConfirmationHandler: Send + Sync {
    /// Request confirmation from the user.
    async fn confirm(&self, message: &str) -> Result<ConfirmationAction>;
}

/// Chat client trait for sending requests to chat APIs.
pub trait ChatClient: Send + Sync {
    /// Send a chat request.
    fn send_chat_request(&self, messages: &[Value], options: &ChatRequestOptions) -> Result<Value>;
}

impl ChatClient for ChatGptClient {
    fn send_chat_request(&self, messages: &[Value], options: &ChatRequestOptions) -> Result<Value> {
        self.send_chat(messages, options, None)
    }
}

/// Live implementation of AiService using OpenAI API and MCP tools.
pub struct LiveAiService {
    client: Arc<dyn ChatClient>,
    mcp_manager: Arc<RwLock<McpManager>>,
    safety_level: Arc<RwLock<SafetyLevel>>,
    safety_guard: Arc<SafetyGuard>,
    confirmation_handler: Option<Arc<dyn ConfirmationHandler>>,
    execute_allowlist: Arc<RwLock<Vec<String>>>,
}

impl LiveAiService {
    /// Create a new LiveAiService instance.
    pub fn new(
        client: impl ChatClient + 'static,
        mcp_manager: Arc<RwLock<McpManager>>,
        safety_level: Arc<RwLock<SafetyLevel>>,
        safety_guard: Arc<SafetyGuard>,
        confirmation_handler: Option<Arc<dyn ConfirmationHandler>>,
        execute_allowlist: Arc<RwLock<Vec<String>>>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            mcp_manager,
            safety_level,
            safety_guard,
            confirmation_handler,
            execute_allowlist,
        }
    }
}

#[async_trait]
impl AiService for LiveAiService {
    fn get_safety_guard(&self) -> Option<Arc<SafetyGuard>> {
        Some(self.safety_guard.clone())
    }

    fn get_safety_level(&self) -> Option<SafetyLevel> {
        Some(self.safety_level.read().clone())
    }

    fn get_allowlist(&self) -> Option<Vec<String>> {
        Some(self.execute_allowlist.read().clone())
    }

    async fn send_request(
        &self,
        messages_in: Vec<Value>,
        temperature: Option<f64>,
    ) -> Result<String> {
        self.run_tool_loop(messages_in, AiRequestOptions::new(temperature))
            .await
    }

    async fn send_request_with(
        &self,
        messages_in: Vec<Value>,
        options: AiRequestOptions,
    ) -> Result<String> {
        self.run_tool_loop(messages_in, options).await
    }
}

impl LiveAiService {
    async fn run_tool_loop(
        &self,
        messages_in: Vec<Value>,
        options: AiRequestOptions,
    ) -> Result<String> {
        let mut messages = messages_in;
        let tools = self.mcp_manager.read().tool_definitions();
        let chat_options = options.to_chat_options((!tools.is_empty()).then(|| tools.clone()));

        let mut iterations = 0;
        // Rounds where the model produced neither a tool call nor an answer.
        let mut stalled_rounds = 0usize;
        const MAX_ITERATIONS: usize = 10;

        loop {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                anyhow::bail!("AI request exceeded maximum number of tool interactions");
            }

            let response = self.client.send_chat_request(&messages, &chat_options)?;

            // Shared with the `!` chat runtime so the two loops cannot drift.
            let interpreted = turn::interpret_response(&response)
                .map_err(|err| anyhow::anyhow!("Failed to parse AI response: {err}"))?;

            let tool_calls = match interpreted.outcome {
                TurnOutcome::ToolCalls(tool_calls) => tool_calls,
                TurnOutcome::Answer(content) => return Ok(content.trim().to_string()),
                TurnOutcome::Cut {
                    finish_reason,
                    partial,
                } => {
                    anyhow::bail!(
                        "AI request stopped early: {}",
                        turn::describe_cut(&finish_reason, partial.as_deref())
                    );
                }
                TurnOutcome::Stalled => {
                    // Resending the identical request only burns the budget.
                    stalled_rounds += 1;
                    if stalled_rounds > 1 {
                        anyhow::bail!(
                            "AI request returned neither a tool call nor an answer twice in a row"
                        );
                    }
                    messages.push(json!({
                        "role": "user",
                        "content": "Your last reply contained neither a tool call nor an answer. Either call a tool or answer the question now."
                    }));
                    continue;
                }
            };

            {
                stalled_rounds = 0;
                if let Some(assistant_message) = interpreted.assistant_message {
                    messages.push(assistant_message);
                }

                for tool_call in &tool_calls {
                    let id = tool_call
                        .get("id")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default();
                    let function = tool_call.get("function").and_then(|v| v.as_object());
                    let name = function
                        .and_then(|f| f.get("name"))
                        .and_then(|s| s.as_str())
                        .unwrap_or_default();
                    let args = function
                        .and_then(|f| f.get("arguments"))
                        .and_then(|s| s.as_str())
                        .unwrap_or_default();

                    // Check safety
                    let allowlist = self.execute_allowlist.read().clone();
                    let level = self.safety_level.read().clone();

                    let result = self
                        .safety_guard
                        .check_mcp_tool(name, args, &level, &allowlist);
                    match result {
                        SafetyResult::Allowed => {}
                        SafetyResult::Denied(msg) => {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": format!("Tool execution denied by safety policy: {}", msg)
                            }));
                            continue;
                        }
                        SafetyResult::Confirm(msg) => {
                            let Some(handler) = &self.confirmation_handler else {
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "Tool execution requires user confirmation, but confirmation is unavailable in this context"
                                }));
                                continue;
                            };

                            match handler.confirm(&msg).await? {
                                ConfirmationAction::Yes => {
                                    // Proceed
                                }
                                ConfirmationAction::AlwaysAllow => {
                                    let mut list = self.execute_allowlist.write();
                                    let entry = SafetyGuard::mcp_allowlist_entry(name, args);
                                    if !list.contains(&entry) {
                                        list.push(entry);
                                    }
                                }
                                ConfirmationAction::No => {
                                    messages.push(json!({
                                        "role": "tool",
                                        "tool_call_id": id,
                                        "content": "User rejected tool execution"
                                    }));
                                    continue;
                                }
                            }
                        }
                    }

                    // Execute tool
                    let result_str = match self.mcp_manager.read().execute_tool(name, args) {
                        Ok(Some(res)) => res,
                        Ok(None) => "Tool executed successfully (no output)".to_string(),
                        Err(e) => format!("Error executing tool: {}", e),
                    };

                    // An MCP tool can return a whole file or log; unbounded, it
                    // grows every following request in this loop.
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": turn::truncate_middle(&result_str, MAX_TOOL_OUTPUT_CHARS)
                    }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Answers every request with a reply that has neither tool calls nor
    /// content - the shape that used to be resent until the iteration cap.
    struct StallingClient {
        calls: Arc<AtomicUsize>,
    }

    impl ChatClient for StallingClient {
        fn send_chat_request(
            &self,
            _messages: &[Value],
            _options: &ChatRequestOptions,
        ) -> Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({
                "choices": [{ "message": { "role": "assistant" }, "finish_reason": "tool_calls" }]
            }))
        }
    }

    struct EchoClient;

    impl ChatClient for EchoClient {
        fn send_chat_request(
            &self,
            _messages: &[Value],
            options: &ChatRequestOptions,
        ) -> Result<Value> {
            Ok(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": format!(
                            "cap={:?} json={}",
                            options.max_tokens,
                            options.response_format.is_some()
                        )
                    },
                    "finish_reason": "stop"
                }]
            }))
        }
    }

    fn service(client: impl ChatClient + 'static) -> LiveAiService {
        LiveAiService::new(
            client,
            Arc::new(RwLock::new(McpManager::default())),
            Arc::new(RwLock::new(SafetyLevel::Normal)),
            Arc::new(SafetyGuard::new()),
            None,
            Arc::new(RwLock::new(Vec::new())),
        )
    }

    #[tokio::test]
    async fn a_stalling_model_stops_after_one_nudge() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = service(StallingClient {
            calls: calls.clone(),
        });

        let result = service
            .send_request(vec![json!({"role": "user", "content": "hi"})], Some(0.1))
            .await;

        assert!(result.is_err());
        // One request, one nudged retry - not the 10-iteration cap.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_bounded_request_forwards_its_token_cap() {
        let service = service(EchoClient);

        let answer = service
            .send_request_with(
                vec![json!({"role": "user", "content": "hi"})],
                AiRequestOptions::new(Some(0.1))
                    .with_max_tokens(Some(64))
                    .as_json_object(),
            )
            .await
            .unwrap();

        assert_eq!(answer, "cap=Some(64) json=true");
    }

    #[tokio::test]
    async fn an_unbounded_request_sends_no_cap() {
        let service = service(EchoClient);

        let answer = service
            .send_request(vec![json!({"role": "user", "content": "hi"})], Some(0.1))
            .await
            .unwrap();

        assert_eq!(answer, "cap=None json=false");
    }
}
