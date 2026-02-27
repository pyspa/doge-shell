//! AI-powered command analysis and diagnostics.
//!
//! This module provides functions for explaining commands, checking safety,
//! suggesting improvements, and diagnosing command output.

use super::service::AiService;
use crate::safety::{PromptInjectionResult, SafetyGuard};
use anyhow::Result;
use serde_json::json;

/// Explain a shell command in natural language.
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

/// Explain a shell command briefly in a single line (for inline ghost text).
pub async fn explain_command_inline<S: AiService + ?Sized>(
    service: &S,
    command: &str,
) -> Result<String> {
    if let PromptInjectionResult::Suspicious(warnings) =
        SafetyGuard::check_prompt_injection(command)
    {
        tracing::warn!(
            "Potential prompt injection in explain_command_inline: {:?}",
            warnings
        );
    }
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 200);

    let system_prompt = "You are a shell command expert. Explain the given command briefly in a single line. \
    Do NOT use Markdown formatting (like `backticks` or **bold**). Keep it under 60 characters if possible. \
    Respond in the same language as the user's environment if possible, or match the language of their request.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Explain this briefly:\n{}", sanitized_command)}),
    ];

    service.send_request(messages, Some(0.1)).await
}

/// Suggest improvements for a shell command.
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

/// Check if a command is potentially dangerous.
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

/// Diagnose command output (especially errors).
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

/// Diagnose command output and return both the response and the conversation history.
pub async fn diagnose_output_with_history<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    exit_code: i32,
) -> Result<(String, Vec<serde_json::Value>)> {
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    // Output can be huge, sanitize and truncate
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 2000);

    let system_prompt = "You are a debugging expert. Analyze the command output and diagnose any issues. \
    Focus on error messages and their root causes. Provide clear, actionable solutions. \
    Respond in the same language as the user's environment if possible, or match the language of their request. \
    Additionally, output markdown bash code blocks (```bash ... ```) when proposing commands, \
    so the user can easily copy or apply them.";

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

    let mut messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let response = service.send_request(messages.clone(), Some(0.2)).await?;
    messages.push(json!({"role": "assistant", "content": response}));

    Ok((response, messages))
}

/// Send a followup question leveraging existing conversation history.
pub async fn send_followup_question<S: AiService + ?Sized>(
    service: &S,
    history: &mut Vec<serde_json::Value>,
    query: &str,
) -> Result<String> {
    if let PromptInjectionResult::Suspicious(warnings) = SafetyGuard::check_prompt_injection(query)
    {
        tracing::warn!(
            "Potential prompt injection in followup_question: {:?}",
            warnings
        );
    }
    let sanitized_query = SafetyGuard::sanitize_ai_input(query, 1000);

    history.push(json!({"role": "user", "content": sanitized_query}));

    let response = service.send_request(history.clone(), Some(0.2)).await?;
    history.push(json!({"role": "assistant", "content": response.clone()}));

    Ok(response)
}

/// Analyze command output with AI based on a user query.
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
