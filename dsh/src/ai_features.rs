use anyhow::Result;
use async_trait::async_trait;
use dsh_builtin::McpManager;
use dsh_openai::ChatGptClient;
use serde_json::{Value, json};
use std::sync::Arc;

#[async_trait]
pub trait AiService: Send + Sync {
    async fn send_request(&self, messages: Vec<Value>, temperature: Option<f64>) -> Result<String>;
}

use parking_lot::RwLock;

use crate::safety::SafetyLevel;

#[async_trait]
pub trait ConfirmationHandler: Send + Sync {
    async fn confirm(&self, message: &str) -> Result<bool>;
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
        self.send_chat_request(messages, temperature, model, tools)
    }
}

pub struct LiveAiService {
    client: Arc<dyn ChatClient>,
    mcp_manager: Arc<RwLock<McpManager>>,
    safety_level: Arc<RwLock<SafetyLevel>>,
    confirmation_handler: Option<Arc<dyn ConfirmationHandler>>,
}

impl LiveAiService {
    pub fn new(
        client: impl ChatClient + 'static,
        mcp_manager: Arc<RwLock<McpManager>>,
        safety_level: Arc<RwLock<SafetyLevel>>,
        confirmation_handler: Option<Arc<dyn ConfirmationHandler>>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            mcp_manager,
            safety_level,
            confirmation_handler,
        }
    }
}

#[async_trait]
impl AiService for LiveAiService {
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
                    let safety_level = self.safety_level.read().clone();
                    let should_confirm = match safety_level {
                        SafetyLevel::Strict | SafetyLevel::Normal => true,
                        SafetyLevel::Loose => false,
                    };

                    if should_confirm
                        && let Some(handler) = &self.confirmation_handler {
                            let msg = format!("[MCP] Execute tool '{}'? Args: {}", name, args);
                            if !handler.confirm(&msg).await? {
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "User rejected tool execution"
                                }));
                                continue;
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
    } else {
        content.to_string()
    }
}

pub async fn expand_smart_pipe<S: AiService + ?Sized>(service: &S, query: &str) -> Result<String> {
    let system_prompt = "You are a shell command expert. The user wants to extend a shell pipeline. \
    Given the user's natural language query, output ONLY the next command(s) in the pipeline starting with a command name (e.g. 'grep', 'awk', 'jq'). \
    Do not output the pipe symbol '|' at the beginning. Do not output markdown code blocks. Output ONLY the command code.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    Ok(sanitize_code_block(&content))
}

pub async fn run_generative_command<S: AiService + ?Sized>(
    service: &S,
    query: &str,
) -> Result<String> {
    let system_prompt = "You are a shell command expert. Convert the following natural language request into a single-line shell command. \
    Output ONLY the command code. Do not include markdown code blocks. Do not include explanations.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    Ok(sanitize_code_block(&content))
}

pub async fn fix_command<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    exit_code: i32,
) -> Result<String> {
    let system_prompt = "You are a shell command expert. The user executed a command that failed. \
    Given the failed command and its exit code, output ONLY the corrected command. \
    Do not output markdown code blocks. Output ONLY the command code. \
    If you are unsure, output the original command.";

    let query = format!("Command: `{}`\nExit Code: {}", command, exit_code);

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    Ok(sanitize_code_block(&content))
}

pub async fn generate_commit_message<S: AiService + ?Sized>(
    service: &S,
    diff: &str,
) -> Result<String> {
    let system_prompt = "You are an expert developer. Generate a concise and descriptive git commit message based on the provided diff. \
    Follow the Conventional Commits specification if possible (e.g., 'feat: ...', 'fix: ...'). \
    Output ONLY the commit message. Do not include markdown code blocks. \
    The message should be a single line if it's short, or a summary line followed by a blank line and details if complex. \
    However, for this interaction, prefer a single line summary.";

    // Truncate diff if it's too long to avoid token limits (rudimentary handling)
    let truncated_diff = if diff.len() > 8000 {
        format!("{}\\n...(truncated)", &diff[..8000])
    } else {
        diff.to_string()
    };

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": truncated_diff}),
    ];

    let content = service.send_request(messages, Some(0.3)).await?;
    Ok(sanitize_code_block(&content))
}

/// Explain a shell command in natural language
pub async fn explain_command<S: AiService + ?Sized>(service: &S, command: &str) -> Result<String> {
    let system_prompt = "You are a shell command expert. Explain the given command in a clear and concise way. \
    Break down each part of the command (command name, options, arguments). \
    Keep the explanation brief but informative. Use markdown formatting for clarity.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Explain this command:\n```\n{}\n```", command)}),
    ];

    service.send_request(messages, Some(0.2)).await
}

/// Suggest improvements for a shell command
pub async fn suggest_improvement<S: AiService + ?Sized>(
    service: &S,
    command: &str,
) -> Result<String> {
    let system_prompt = "You are a shell command expert. Analyze the given command and suggest improvements. \
    Consider: efficiency, readability, safety, and best practices. \
    If the command is already optimal, say so. Provide the improved command if applicable. \
    Keep your response concise and practical.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Suggest improvements for:\n```\n{}\n```", command)}),
    ];

    service.send_request(messages, Some(0.3)).await
}

/// Check if a command is potentially dangerous
pub async fn check_safety<S: AiService + ?Sized>(service: &S, command: &str) -> Result<String> {
    let system_prompt = "You are a security-conscious shell expert. Analyze the given command for potential dangers. \
    Check for: destructive operations (rm -rf), permission issues, data loss risks, security vulnerabilities. \
    Rate the risk level (Safe/Caution/Dangerous) and explain why. Be concise but thorough.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Check safety of:\n```\n{}\n```", command)}),
    ];

    service.send_request(messages, Some(0.1)).await
}

/// Diagnose command output (especially errors)
pub async fn diagnose_output<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    exit_code: i32,
) -> Result<String> {
    let system_prompt = "You are a debugging expert. Analyze the command output and diagnose any issues. \
    Focus on error messages and their root causes. Provide clear, actionable solutions. \
    If the output indicates success, confirm it briefly.";

    // Truncate output if too long
    let truncated_output = if output.len() > 4000 {
        format!("{}...(truncated)", &output[..4000])
    } else {
        output.to_string()
    };

    let query = format!(
        "Command: `{}`\nExit code: {}\nOutput:\n```\n{}\n```",
        command, exit_code, truncated_output
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service.send_request(messages, Some(0.2)).await
}

/// Describe the current directory structure
pub async fn describe_directory<S: AiService + ?Sized>(
    service: &S,
    dir_listing: &str,
    cwd: &str,
) -> Result<String> {
    let system_prompt = "You are a project analyst. Based on the directory listing, describe what type of project this is. \
    Identify the technology stack, framework, and purpose if possible. \
    Suggest relevant commands the user might want to run. Be concise.";

    let query = format!(
        "Current directory: {}\n\nFiles:\n```\n{}\n```",
        cwd, dir_listing
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service.send_request(messages, Some(0.3)).await
}

/// Suggest next commands based on context
pub async fn suggest_next_commands<S: AiService + ?Sized>(
    service: &S,
    recent_commands: &[String],
    cwd: &str,
    dir_listing: &str,
) -> Result<String> {
    let system_prompt = "You are a helpful shell assistant. Based on the user's recent commands and current context, \
    suggest 3-5 useful commands they might want to run next. \
    Format as a numbered list. Be practical and context-aware.";

    let recent = if recent_commands.is_empty() {
        "None".to_string()
    } else {
        recent_commands.join("\n")
    };

    let query = format!(
        "Recent commands:\n{}\n\nCurrent directory: {}\n\nFiles:\n```\n{}\n```",
        recent, cwd, dir_listing
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service.send_request(messages, Some(0.4)).await
}

/// Analyze command output with AI based on a user query
pub async fn analyze_output<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    query: &str,
) -> Result<String> {
    let system_prompt = "You are a shell output analyst. \
    Analyze the following command output and respond to the user's query. \
    Be concise and practical. Use markdown formatting for clarity.";

    // Truncate output if too long to avoid token limits
    let truncated_output = if output.len() > 8000 {
        format!("{}...(truncated)", &output[..8000])
    } else {
        output.to_string()
    };

    let user_message = format!(
        "Command: `{}`\n\nOutput:\n```\n{}\n```\n\nQuery: {}",
        command, truncated_output, query
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": user_message}),
    ];

    service.send_request(messages, Some(0.2)).await
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
        let service = MockAiService::new("grep foo");
        let result = expand_smart_pipe(&service, "extract foo").await.unwrap();
        assert_eq!(result, "grep foo");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "extract foo");
    }

    #[tokio::test]
    async fn test_run_generative_command() {
        let service = MockAiService::new("git status");
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

    #[tokio::test]
    async fn test_fix_command() {
        let service = MockAiService::new("ls -la");
        let result = fix_command(&service, "lss -la", 127).await.unwrap();
        assert_eq!(result, "ls -la");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"].as_str().unwrap().contains("lss -la"));
    }

    #[tokio::test]
    async fn test_generate_commit_message() {
        let service = MockAiService::new("feat: add new feature");
        let diff = "diff --git a/file.txt b/file.txt...";
        let result = generate_commit_message(&service, diff).await.unwrap();
        assert_eq!(result, "feat: add new feature");
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

    // Mocks for LiveAiService tests
    struct MockChatClient {
        tool_call_response: Value,
        final_response: Value,
        call_count: Mutex<usize>,
    }

    impl MockChatClient {
        fn new(tool_name: &str) -> Self {
            let tool_call = json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_123",
                            "type": "function",
                            "function": {
                                "name": tool_name,
                                "arguments": "{}"
                            }
                        }]
                    }
                }]
            });

            let final_resp = json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "Done"
                    }
                }]
            });

            Self {
                tool_call_response: tool_call,
                final_response: final_resp,
                call_count: Mutex::new(0),
            }
        }
    }

    impl ChatClient for MockChatClient {
        fn send_chat_request(
            &self,
            _messages: &[Value],
            _temperature: Option<f64>,
            _model: Option<String>,
            _tools: Option<&[Value]>,
        ) -> Result<Value> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;
            if *count == 1 {
                Ok(self.tool_call_response.clone())
            } else {
                Ok(self.final_response.clone())
            }
        }
    }

    struct MockConfirmation {
        should_confirm: bool,
        called: Arc<Mutex<bool>>,
    }

    #[async_trait]
    impl ConfirmationHandler for MockConfirmation {
        async fn confirm(&self, _message: &str) -> Result<bool> {
            *self.called.lock().unwrap() = true;
            Ok(self.should_confirm)
        }
    }

    #[tokio::test]
    async fn test_confirmation_call_strict() {
        let client = MockChatClient::new("test_tool");
        let mcp_manager = Arc::new(RwLock::new(McpManager::load(vec![])));
        let safety_level = Arc::new(RwLock::new(SafetyLevel::Strict));
        let confirmed_called = Arc::new(Mutex::new(false));
        let confirmation_handler = Arc::new(MockConfirmation {
            should_confirm: true,
            called: confirmed_called.clone(),
        });

        let service = LiveAiService::new(
            client,
            mcp_manager,
            safety_level,
            Some(confirmation_handler),
        );

        let _ = service
            .send_request(vec![json!({"role": "user", "content": "hi"})], None)
            .await;

        assert!(*confirmed_called.lock().unwrap());
    }

    #[tokio::test]
    async fn test_confirmation_not_called_loose() {
        let client = MockChatClient::new("test_tool");
        let mcp_manager = Arc::new(RwLock::new(McpManager::load(vec![])));
        let safety_level = Arc::new(RwLock::new(SafetyLevel::Loose));
        let confirmed_called = Arc::new(Mutex::new(false));
        let confirmation_handler = Arc::new(MockConfirmation {
            should_confirm: true,
            called: confirmed_called.clone(),
        });

        let service = LiveAiService::new(
            client,
            mcp_manager,
            safety_level,
            Some(confirmation_handler),
        );

        let _ = service
            .send_request(vec![json!({"role": "user", "content": "hi"})], None)
            .await;

        assert!(!*confirmed_called.lock().unwrap());
    }

    #[tokio::test]
    async fn test_confirmation_rejection() {
        let client = MockChatClient::new("test_tool");
        let mcp_manager = Arc::new(RwLock::new(McpManager::load(vec![])));
        let safety_level = Arc::new(RwLock::new(SafetyLevel::Strict));
        let confirmed_called = Arc::new(Mutex::new(false));
        let confirmation_handler = Arc::new(MockConfirmation {
            should_confirm: false,
            called: confirmed_called.clone(),
        });

        let service = LiveAiService::new(
            client,
            mcp_manager,
            safety_level,
            Some(confirmation_handler),
        );

        let res = service
            .send_request(vec![json!({"role": "user", "content": "hi"})], None)
            .await
            .unwrap();

        assert!(*confirmed_called.lock().unwrap());
        assert_eq!(res, "Done");
    }
}
