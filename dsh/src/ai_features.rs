use crate::safety::{PromptInjectionResult, SafetyGuard, SafetyLevel, SafetyResult};
use anyhow::Result;
use async_trait::async_trait;
use dsh_builtin::McpManager;
use dsh_openai::ChatGptClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct AiCommandResponse {
    command: String,
    args: Vec<String>,
}

#[async_trait]
pub trait AiService: Send + Sync {
    async fn send_request(&self, messages: Vec<Value>, temperature: Option<f64>) -> Result<String>;

    fn get_safety_guard(&self) -> Option<Arc<SafetyGuard>> {
        None
    }
    fn get_safety_level(&self) -> Option<SafetyLevel> {
        None
    }
    fn get_allowlist(&self) -> Option<Vec<String>> {
        None
    }
}

use parking_lot::RwLock;

use crate::repl::confirmation::ConfirmationAction;

#[async_trait]
pub trait ConfirmationHandler: Send + Sync {
    async fn confirm(&self, message: &str) -> Result<ConfirmationAction>;
}

pub trait ChatClient: Send + Sync {
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

pub struct LiveAiService {
    client: Arc<dyn ChatClient>,
    mcp_manager: Arc<RwLock<McpManager>>,
    safety_level: Arc<RwLock<SafetyLevel>>,
    safety_guard: Arc<SafetyGuard>,
    confirmation_handler: Option<Arc<dyn ConfirmationHandler>>,
    execute_allowlist: Arc<RwLock<Vec<String>>>,
}

impl LiveAiService {
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
                                // Need to extract the inner command again to add to list
                                // This duplicates logic inside check_mcp_tool but we need the exact string
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

pub fn sanitize_code_block(content: &str) -> String {
    let content = content.trim_matches(|c| c == '`');
    if let Some(stripped) = content.strip_prefix("bash\n") {
        stripped.to_string()
    } else if let Some(stripped) = content.strip_prefix("json\n") {
        stripped.to_string()
    } else {
        content.to_string()
    }
}

pub async fn expand_smart_pipe<S: AiService + ?Sized>(service: &S, query: &str) -> Result<String> {
    // Check for potential prompt injection
    if let PromptInjectionResult::Suspicious(warnings) = SafetyGuard::check_prompt_injection(query)
    {
        tracing::warn!(
            "Potential prompt injection detected in smart pipe: {:?}",
            warnings
        );
    }

    // Sanitize input
    let sanitized_query = SafetyGuard::sanitize_ai_input(query, 2000);

    let system_prompt = "You are a shell command expert. The user wants to extend a shell pipeline. \
    Given the user's natural language query, output the next command in the pipeline as a JSON object. \
    Format: {\"command\": \"grep\", \"args\": [\"-r\", \"pattern\"]}. \
    Do not output the pipe symbol '|'. Do not output markdown code blocks. Output ONLY the JSON object.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": sanitized_query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    let content_clean = sanitize_code_block(&content);

    // Parse JSON
    let response: AiCommandResponse = serde_json::from_str(&content_clean).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse AI response as JSON: {}. Content: {}",
            e,
            content_clean
        )
    })?;

    // Check safety
    let safety_guard = service.get_safety_guard();
    let safety_level = service.get_safety_level();
    let allowlist = service.get_allowlist();

    if let (Some(guard), Some(level), Some(allowlist)) = (safety_guard, safety_level, allowlist) {
        match guard.check_command(&level, &response.command, &response.args, &allowlist) {
            SafetyResult::Allowed => {
                // Reconstruct command string
                let mut parts = vec![response.command];
                parts.extend(response.args);
                Ok(parts.join(" "))
            }
            SafetyResult::Denied(msg) | SafetyResult::Confirm(msg) => {
                tracing::warn!("Blocked dangerous AI suggestion: {}", msg);
                Err(anyhow::anyhow!(
                    "Security Warning: AI suggested a potentially dangerous command: {}. {}",
                    response.command,
                    msg
                ))
            }
        }
    } else {
        // Fallback if safety components are not available (e.g. tests)
        let mut parts = vec![response.command];
        parts.extend(response.args);
        Ok(parts.join(" "))
    }
}

pub async fn run_generative_command<S: AiService + ?Sized>(
    service: &S,
    query: &str,
) -> Result<String> {
    // Check for potential prompt injection
    if let PromptInjectionResult::Suspicious(warnings) = SafetyGuard::check_prompt_injection(query)
    {
        tracing::warn!(
            "Potential prompt injection detected in generative command: {:?}",
            warnings
        );
    }

    // Sanitize input
    let sanitized_query = SafetyGuard::sanitize_ai_input(query, 5000);

    let system_prompt = "You are a shell command expert. Convert the following natural language request into a single-line shell command. \
    Target platform: Linux with bash/zsh. \
    Output the result as a JSON object. \
    Format: {\"command\": \"rm\", \"args\": [\"-rf\", \"/\"]}. \
    Do not output markdown code blocks. Output ONLY the JSON object.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": sanitized_query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    let content_clean = sanitize_code_block(&content);

    // Parse JSON
    let response: AiCommandResponse = serde_json::from_str(&content_clean).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse AI response as JSON: {}. Content: {}",
            e,
            content_clean
        )
    })?;

    // Check safety
    let safety_guard = service.get_safety_guard();
    let safety_level = service.get_safety_level();
    let allowlist = service.get_allowlist();

    if let (Some(guard), Some(level), Some(allowlist)) = (safety_guard, safety_level, allowlist) {
        match guard.check_command(&level, &response.command, &response.args, &allowlist) {
            SafetyResult::Allowed => {
                // Reconstruct command string
                let mut parts = vec![response.command];
                parts.extend(response.args);
                Ok(parts.join(" "))
            }
            SafetyResult::Denied(msg) | SafetyResult::Confirm(msg) => {
                tracing::warn!("Blocked dangerous AI generated command: {}", msg);
                Err(anyhow::anyhow!(
                    "Security Warning: AI generated a potentially dangerous command: {}. {}",
                    response.command,
                    msg
                ))
            }
        }
    } else {
        // Fallback for tests
        let mut parts = vec![response.command];
        parts.extend(response.args);
        Ok(parts.join(" "))
    }
}

pub async fn fix_command<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    exit_code: i32,
    output: &str,
) -> Result<String> {
    // Check injection on command (unlikely but possible source)
    if let PromptInjectionResult::Suspicious(warnings) =
        SafetyGuard::check_prompt_injection(command)
    {
        tracing::warn!(
            "Potential prompt injection in fix_command source: {:?}",
            warnings
        );
    }

    // Sanitize inputs
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    // Output can be large and contain anything, sanitize it but allow standard chars
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 2000);

    let system_prompt = "You are a shell command expert. The user executed a command that failed. \
    Given the failed command, its exit code, and its output (including potential error messages), \
    output the corrected command as a JSON object. \
    Format: {\"command\": \"grep\", \"args\": [\"-r\", \"pattern\"]}. \
    Do not output markdown code blocks. Output ONLY the JSON object. \
    If you cannot determine a fix, output the original command inside the JSON.";

    let query = format!(
        "Failed command: `{}`\nExit code: {}\nOutput:\n```\n{}\n```",
        sanitized_command, exit_code, sanitized_output
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    let content_clean = sanitize_code_block(&content);

    // Parse JSON
    let response: AiCommandResponse = serde_json::from_str(&content_clean).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse AI response as JSON: {}. Content: {}",
            e,
            content_clean
        )
    })?;

    // Check safety
    let safety_guard = service.get_safety_guard();
    let safety_level = service.get_safety_level();
    let allowlist = service.get_allowlist();

    if let (Some(guard), Some(level), Some(allowlist)) = (safety_guard, safety_level, allowlist) {
        match guard.check_command(&level, &response.command, &response.args, &allowlist) {
            SafetyResult::Allowed => {
                // Reconstruct command string
                let mut parts = vec![response.command];
                parts.extend(response.args);
                Ok(parts.join(" "))
            }
            SafetyResult::Denied(msg) | SafetyResult::Confirm(msg) => {
                tracing::warn!("Blocked dangerous AI fix suggestion: {}", msg);
                Err(anyhow::anyhow!(
                    "Security Warning: AI suggested a potentially dangerous fix: {}. {}",
                    response.command,
                    msg
                ))
            }
        }
    } else {
        // Fallback
        let mut parts = vec![response.command];
        parts.extend(response.args);
        Ok(parts.join(" "))
    }
}

/// Explain a shell command in natural language
pub async fn explain_command<S: AiService + ?Sized>(service: &S, command: &str) -> Result<String> {
    if let PromptInjectionResult::Suspicious(warnings) =
        SafetyGuard::check_prompt_injection(command)
    {
        tracing::warn!(
            "Potential prompt injection in explain_command: {:?}",
            warnings
        );
    }
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 2000);

    let system_prompt = "You are a shell command expert. Explain the given command in a clear and concise way. \
    Break down each part of the command (command name, options, arguments). \
    Keep the explanation brief but informative. Use markdown formatting for clarity. \
    Respond in the same language as the user's request (e.g., if they ask in Japanese, explain in Japanese).";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Explain this command:\n```\n{}\n```", sanitized_command)}),
    ];

    service.send_request(messages, Some(0.2)).await
}

/// Suggest improvements for a shell command
/// Suggest improvements for a shell command
pub async fn suggest_improvement<S: AiService + ?Sized>(
    service: &S,
    command: &str,
) -> Result<String> {
    if let PromptInjectionResult::Suspicious(warnings) =
        SafetyGuard::check_prompt_injection(command)
    {
        tracing::warn!(
            "Potential prompt injection in suggest_improvement: {:?}",
            warnings
        );
    }
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 2000);

    let system_prompt = "You are a shell command expert. Suggest improvements for the given command if any. \
    Consider safety, performance, and best practices. \
    If the command is already optimal, say so. \
    Respond in the same language as the user's request.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Suggest improvements for:\n```\n{}\n```", sanitized_command)}),
    ];

    service.send_request(messages, Some(0.3)).await
}

/// Check if a command is potentially dangerous
/// Check if a command is potentially dangerous
pub async fn check_safety<S: AiService + ?Sized>(service: &S, command: &str) -> Result<String> {
    // Does not need heavy sanitization as it is security check itself, but prevent injection
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 2000);

    let system_prompt = "You are a security-conscious shell expert. Analyze the given command for potential security risks. \
    If the command is dangerous (e.g., deletes files, modifies system settings, sends data externally), explain the risk. \
    Output 'SAFE' if the command appears safe. \
    Output 'WARNING: <reason>' if there are risks.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Check safety of:\n```\n{}\n```", sanitized_command)}),
    ];

    service.send_request(messages, Some(0.1)).await
}

/// Diagnose command output (especially errors)
/// Diagnose command output (especially errors)
pub async fn diagnose_output<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    exit_code: i32,
) -> Result<String> {
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    // Output can be huge, sanitize and truncate
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 2000);

    let system_prompt = "You are a debugging expert. Analyze the command output and diagnose any issues. \
    Focus on error messages and their root causes. Provide clear, actionable solutions. \
    Respond in the same language as the user's environment if possible, or match the language of their request.";

    // Truncate output if too long
    let truncated_output = if sanitized_output.len() > 4000 {
        format!("{}...(truncated)", &sanitized_output[..4000])
    } else {
        sanitized_output.to_string()
    };

    let query = format!(
        "Command: `{}`\nExit code: {}\nOutput:\n```\n{}\n```",
        sanitized_command, exit_code, truncated_output
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service.send_request(messages, Some(0.2)).await
}

/// Describe the current directory structure
/// Describe the current directory structure
pub async fn describe_directory<S: AiService + ?Sized>(
    service: &S,
    dir_listing: &str,
    cwd: &str,
) -> Result<String> {
    let sanitized_cwd = SafetyGuard::sanitize_ai_input(cwd, 500);
    let sanitized_listing = SafetyGuard::sanitize_ai_input(dir_listing, 3000);

    let system_prompt = "You are a project analyst. Based on the directory listing, describe what type of project this is. \
    Identify the technology stack, framework, and purpose if possible. \
    Suggest relevant commands the user might want to run. Be concise.";

    let query = format!(
        "Current directory: {}\n\nFiles:\n```\n{}\n```",
        sanitized_cwd, sanitized_listing
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service.send_request(messages, Some(0.3)).await
}

/// Suggest next commands based on context
/// Suggest next commands based on context
pub async fn suggest_next_commands<S: AiService + ?Sized>(
    service: &S,
    recent_commands: &[String],
    cwd: &str,
    dir_listing: &str,
) -> Result<String> {
    let sanitized_cwd = SafetyGuard::sanitize_ai_input(cwd, 500);
    let sanitized_listing = SafetyGuard::sanitize_ai_input(dir_listing, 2000);
    // Sanitize recent commands
    let sanitized_history: Vec<String> = recent_commands
        .iter()
        .map(|c| SafetyGuard::sanitize_ai_input(c, 200))
        .collect();

    let system_prompt = "You are a helpful shell assistant. Based on the user's recent commands and current context, \
    suggest 3-5 useful commands they might want to run next. \
    Format as a numbered list. Be practical and context-aware.";

    let recent = if sanitized_history.is_empty() {
        "None".to_string()
    } else {
        sanitized_history.join("\n")
    };

    let query = format!(
        "Recent commands:\n{}\n\nCurrent directory: {}\n\nFiles:\n```\n{}\n```",
        recent, sanitized_cwd, sanitized_listing
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service.send_request(messages, Some(0.4)).await
}

/// Analyze command output with AI based on a user query
/// Analyze command output with AI based on a user query
pub async fn analyze_output<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    query: &str,
) -> Result<String> {
    if let PromptInjectionResult::Suspicious(warnings) = SafetyGuard::check_prompt_injection(query)
    {
        tracing::warn!(
            "Potential prompt injection in analyze_output: {:?}",
            warnings
        );
    }
    let sanitized_query = SafetyGuard::sanitize_ai_input(query, 1000);
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 2000);

    let system_prompt = "You are a shell output analyst. \
    Analyze the following command output and respond to the user's query. \
    Be concise and practical. Use markdown formatting for clarity. \
    Respond in the same language as the user's query.";

    // Truncate output if too long to avoid token limits
    let truncated_output = if sanitized_output.len() > 8000 {
        format!("{}...(truncated)", &sanitized_output[..8000])
    } else {
        sanitized_output.to_string()
    };

    let user_message = format!(
        "Command: `{}`\n\nOutput:\n```\n{}\n```\n\nQuery: {}",
        sanitized_command, truncated_output, sanitized_query
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": user_message}),
    ];

    service.send_request(messages, Some(0.2)).await
}

pub async fn generate_completion_json<S: AiService + ?Sized>(
    service: &S,
    command_name: &str,
    help_text: &str,
) -> Result<String> {
    let system_prompt = r#"You are a shell command completion definition generator for doge-shell.
Your task is to analyze the provided help text of a command and generate a JSON completion definition.

Output JSON format (strictly valid JSON):
{
  "command": "command_name",
  "description": "Command description",
  "global_options": [
    {
      "short": "-s",
      "long": "--long",
      "description": "Description",
      "takes_value": boolean
    }
  ],
  "subcommands": [
    {
      "name": "subcommand",
      "description": "Description",
      "options": [],
      "arguments": [],
      "subcommands": []
    }
  ]
}

Argument types (use in the "type" field of each argument; MUST be an object):
- File: {"type": "File"} or {"type": "File", "data": {"extensions": [".rs", ".toml"]}}
- Directory: {"type": "Directory"}
- String: {"type": "String"}
- Number: {"type": "Number"}
- Choice: {"type": "Choice", "data": ["val1", "val2"]}
- Command: {"type": "Command"}
- Environment: {"type": "Environment"}
- Url: {"type": "Url"}
- Process: {"type": "Process"}
- Regex: {"type": "Regex"}
- CommandWithArgs: {"type": "CommandWithArgs"}

CRITICAL RULES:
1. Do NOT use "Script" type. It is unsafe and difficult to get right. Use "Choice" if values are known (e.g. log levels, formats), or "String" / "File" / "Directory" / "Command" as appropriate.
2. Return ONLY the JSON string. Do not include markdown code blocks (```json ... ```).
3. Ensure the JSON is valid. Escape double quotes in descriptions if necessary.
"#;

    let user_message = format!("Command: {}\n\nHelp Text:\n{}", command_name, help_text);

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": user_message}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    // sanitize just in case the AI adds markdown despite instructions
    Ok(sanitize_code_block(&content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockAiService {
        response: String,
        last_messages: Mutex<Vec<Value>>,
    }

    impl MockAiService {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                last_messages: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AiService for MockAiService {
        async fn send_request(
            &self,
            messages: Vec<Value>,
            _temperature: Option<f64>,
        ) -> Result<String> {
            *self.last_messages.lock().unwrap() = messages;
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_expand_smart_pipe() {
        let response = r#"{"command": "grep", "args": ["foo"]}"#;
        let service = MockAiService::new(response);
        let result = expand_smart_pipe(&service, "extract foo").await.unwrap();
        assert_eq!(result, "grep foo");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "extract foo");
    }

    #[tokio::test]
    async fn test_run_generative_command() {
        let response = r#"{"command": "git", "args": ["status"]}"#;
        let service = MockAiService::new(response);
        let result = run_generative_command(&service, "check status")
            .await
            .unwrap();
        assert_eq!(result, "git status");
    }

    #[test]
    fn test_sanitize_code_block() {
        assert_eq!(sanitize_code_block("ls -l"), "ls -l");
        assert_eq!(sanitize_code_block("`ls -l`"), "ls -l");
        assert_eq!(sanitize_code_block("```bash\nls -l\n```"), "ls -l\n");
    }

    #[test]
    fn test_sanitize_code_block_edge_cases() {
        // Empty string
        assert_eq!(sanitize_code_block(""), "");

        // Only backticks
        assert_eq!(sanitize_code_block("``"), "");
        assert_eq!(sanitize_code_block("````"), "");

        // Mixed backticks without bash prefix
        assert_eq!(sanitize_code_block("```\necho test\n```"), "\necho test\n");

        // Single backtick at start
        assert_eq!(sanitize_code_block("`echo hello"), "echo hello");

        // Command with special characters
        assert_eq!(
            sanitize_code_block("`find . -name '*.rs'`"),
            "find . -name '*.rs'"
        );

        // Multiline command
        assert_eq!(
            sanitize_code_block("```bash\necho line1\necho line2\n```"),
            "echo line1\necho line2\n"
        );
    }

    #[tokio::test]
    async fn test_run_generative_command_complex() {
        let response = r#"{"command": "find", "args": [".", "-name", "*.rs", "-exec", "grep", "-l", "TODO", "{}", "+"]}"#;
        let service = MockAiService::new(response);
        let result = run_generative_command(&service, "find all rust files with TODO")
            .await
            .unwrap();
        assert_eq!(result, "find . -name *.rs -exec grep -l TODO {} +");
    }

    #[tokio::test]
    async fn test_run_generative_command_japanese() {
        let response = r#"{"command": "git", "args": ["reset", "--soft", "HEAD~1"]}"#;
        let service = MockAiService::new(response);
        let result = run_generative_command(&service, "最後のコミットを取り消す")
            .await
            .unwrap();
        assert_eq!(result, "git reset --soft HEAD~1");
    }

    #[tokio::test]
    async fn test_fix_command() {
        let response = r#"{"command": "ls", "args": ["-la"]}"#;
        let service = MockAiService::new(response);
        let result = fix_command(&service, "lss -la", 127, "").await.unwrap();
        assert_eq!(result, "ls -la");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"].as_str().unwrap().contains("lss -la"));
    }

    #[tokio::test]
    async fn test_fix_command_with_code_block() {
        // AI might return wrapped in backticks
        let response = r#"```json
{"command": "git", "args": ["status"]}
```"#;
        let service = MockAiService::new(response);
        let result = fix_command(&service, "gti status", 127, "").await.unwrap();
        assert_eq!(result, "git status");
    }

    #[tokio::test]
    async fn test_diagnose_output_truncation() {
        let service = MockAiService::new("Output was truncated due to length");
        let long_output = "x".repeat(5000);
        let result = diagnose_output(&service, "cat file", &long_output, 0).await;

        assert!(result.is_ok());

        let messages = service.last_messages.lock().unwrap();
        let content = messages[1]["content"].as_str().unwrap();
        // Output should be truncated at 4000 chars
        assert!(content.contains("...(truncated)"));
    }

    #[tokio::test]
    async fn test_explain_command() {
        let service = MockAiService::new("This command lists files");
        let result = explain_command(&service, "ls -la").await.unwrap();
        assert_eq!(result, "This command lists files");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert!(messages[1]["content"].as_str().unwrap().contains("ls -la"));
    }

    #[tokio::test]
    async fn test_check_safety() {
        let service = MockAiService::new("**Dangerous**: This command will delete all files");
        let result = check_safety(&service, "rm -rf /").await.unwrap();
        assert!(result.contains("Dangerous"));

        let messages = service.last_messages.lock().unwrap();
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("rm -rf /")
        );
    }

    #[tokio::test]
    async fn test_diagnose_output() {
        let service = MockAiService::new("Command not found. Try installing git.");
        let result = diagnose_output(&service, "gti status", "command not found: gti", 127)
            .await
            .unwrap();
        assert!(result.contains("not found"));

        let messages = service.last_messages.lock().unwrap();
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("Exit code: 127")
        );
    }

    #[tokio::test]
    async fn test_analyze_output() {
        let service =
            MockAiService::new("The connection was refused because the server is not running.");
        let result = analyze_output(
            &service,
            "curl http://localhost:8080",
            "curl: (7) Failed to connect to localhost port 8080: Connection refused",
            "何が問題か教えて",
        )
        .await
        .unwrap();
        assert!(result.contains("refused"));

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("curl http://localhost:8080")
        );
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("Connection refused")
        );
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("何が問題か教えて")
        );
    }

    #[tokio::test]
    async fn test_analyze_output_truncation() {
        let service = MockAiService::new("Output was truncated due to length");

        // Create a very long output > 8000 chars
        let long_output = "x".repeat(10000);
        let result = analyze_output(&service, "cat largefile", &long_output, "summarize").await;

        assert!(result.is_ok());

        // Verify that the output was truncated in the request
        let messages = service.last_messages.lock().unwrap();
        let content = messages[1]["content"].as_str().unwrap();
        assert!(content.contains("...(truncated)"));
        // Should be shorter than original
        assert!(content.len() < 10000);
    }

    #[tokio::test]
    async fn test_analyze_output_empty() {
        let service = MockAiService::new("The command produced no output");
        let result = analyze_output(&service, "true", "", "why is there no output?").await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "The command produced no output");

        let messages = service.last_messages.lock().unwrap();
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("why is there no output?")
        );
    }

    #[tokio::test]
    async fn test_analyze_output_with_stderr() {
        let service = MockAiService::new("The error suggests a permission problem with /etc/hosts");
        let result = analyze_output(
            &service,
            "cat /etc/shadow",
            "cat: /etc/shadow: Permission denied",
            "what went wrong?",
        )
        .await;

        assert!(result.is_ok());
        let messages = service.last_messages.lock().unwrap();
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("Permission denied")
        );
    }

    // Note: To test new LiveAiService logic we need a way to mock SafetyGuard which is a struct.
    // SafetyGuard is not a trait, so mocking it directly is hard unless we make it trait or use a mockable wrapper.
    // For this context, we rely on SafetyGuard's own tests and manually checking LiveAiService compilation.
}
