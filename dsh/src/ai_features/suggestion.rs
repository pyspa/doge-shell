//! AI-powered command suggestions.
//!
//! This module provides functions for suggesting next commands and
//! generating completion definitions.

use super::sanitize_code_block;
use super::service::AiService;
use crate::safety::SafetyGuard;
use anyhow::Result;
use serde_json::json;

/// Suggest next commands based on context.
///
/// Analyzes recent commands and current directory to suggest useful next steps.
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

/// Generate a completion JSON definition for a command.
///
/// Analyzes the help text of a command and generates a JSON completion definition
/// for use in shell completion.
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
