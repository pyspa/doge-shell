//! AI Service traits and implementations.
//!
//! This module defines the core AI service abstraction and the live implementation
//! that integrates with OpenAI API and MCP tools.

use crate::repl::confirmation::ConfirmationAction;
use crate::safety::{SafetyGuard, SafetyLevel, SafetyResult};
use anyhow::Result;
use async_trait::async_trait;
use dsh_builtin::McpManager;
use dsh_openai::ChatGptClient;
use parking_lot::RwLock;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

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
    /// Send a chat request with messages, temperature, model, and optional tools.
    fn send_chat_request(
        &self,
        messages: &[Value],
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<&[Value]>,
    ) -> Result<Value>;
}

impl ChatClient for ChatGptClient {
    fn send_chat_request(
        &self,
        messages: &[Value],
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<&[Value]>,
    ) -> Result<Value> {
        self.send_chat_request(messages, temperature, model, tools, None)
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
        let mut messages = messages_in;
        let tools = self.mcp_manager.read().tool_definitions();
        let tools_arg = if tools.is_empty() {
            None
        } else {
            Some(tools.as_slice())
        };

        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 10;

        loop {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                anyhow::bail!("AI request exceeded maximum number of tool interactions");
            }

            let response =
                self.client
                    .send_chat_request(&messages, temperature, None, tools_arg)?;

            let choice = response
                .get("choices")
                .and_then(|c| c.get(0))
                .ok_or_else(|| anyhow::anyhow!("Failed to parse AI response: no choices"))?;

            let message = choice
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Failed to parse AI response: no message"))?;

            // Check for tool calls
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array())
                && !tool_calls.is_empty()
            {
                // Append assistant message with tool calls
                messages.push(message.clone());

                for tool_call in tool_calls {
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
                    if let SafetyResult::Confirm(msg) = result
                        && let Some(handler) = &self.confirmation_handler
                    {
                        match handler.confirm(&msg).await? {
                            ConfirmationAction::Yes => {
                                // Proceed
                            }
                            ConfirmationAction::AlwaysAllow => {
                                // Add to allowlist
                                if let Ok(json_val) =
                                    serde_json::from_str::<serde_json::Value>(args)
                                    && let Some(cmd_str) =
                                        json_val.get("command").and_then(|v| v.as_str())
                                {
                                    let mut list = self.execute_allowlist.write();
                                    let entry = cmd_str.to_string();
                                    if !list.contains(&entry) {
                                        list.push(entry);
                                    }
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

                    // Execute tool
                    let result_str = match self.mcp_manager.read().execute_tool(name, args) {
                        Ok(Some(res)) => res,
                        Ok(None) => "Tool executed successfully (no output)".to_string(),
                        Err(e) => format!("Error executing tool: {}", e),
                    };

                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": result_str
                    }));
                }
                continue; // Loop again with tool results
            }

            // If no tool calls, return content
            return message
                .get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| anyhow::anyhow!("Failed to parse AI response: no content"));
        }
    }
}
