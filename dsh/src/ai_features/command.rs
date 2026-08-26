//! AI-powered command generation and manipulation.
//!
//! This module provides functions for generating shell commands from natural language,
//! expanding smart pipes, and fixing failed commands.

use super::sanitize_code_block;
use super::service::{AiCommandResponse, AiRequestOptions, AiService};
use crate::safety::{PromptInjectionResult, SafetyGuard, SafetyResult};
use anyhow::Result;

fn command_json_options() -> AiRequestOptions {
    // No token cap: on a reasoning model `max_completion_tokens` also budgets
    // hidden reasoning, so a small cap yields finish_reason=length and no JSON.
    AiRequestOptions::new(Some(0.1)).as_json_object()
}
use serde_json::json;

/// Expand a smart pipe query into a shell command.
///
/// Takes a natural language query and converts it into a command suitable for
/// extending a shell pipeline.
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

    let content = service
        .send_request_with(messages, command_json_options())
        .await?;
    let content_clean = sanitize_code_block(&content);

    // Parse JSON
    let response: AiCommandResponse = serde_json::from_str(&content_clean).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse AI response as JSON: {}. Content: {}",
            e,
            content_clean
        )
    })?;

    finalize_command(service, response, "pipeline suggestion")
}

/// Generate a shell command from a natural language request.
///
/// Converts a natural language description into a complete shell command.
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

    let content = service
        .send_request_with(messages, command_json_options())
        .await?;
    let content_clean = sanitize_code_block(&content);

    // Parse JSON
    let response: AiCommandResponse = serde_json::from_str(&content_clean).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse AI response as JSON: {}. Content: {}",
            e,
            content_clean
        )
    })?;

    finalize_command(service, response, "generated command")
}

/// Fix a failed command using AI analysis.
///
/// Analyzes a failed command and its output to suggest a corrected command.
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

    let content = service
        .send_request_with(messages, command_json_options())
        .await?;
    let content_clean = sanitize_code_block(&content);

    // Parse JSON
    let response: AiCommandResponse = serde_json::from_str(&content_clean).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse AI response as JSON: {}. Content: {}",
            e,
            content_clean
        )
    })?;

    finalize_command(service, response, "fix suggestion")
}

/// Turn a validated AI command response into a command string.
///
/// `Confirm` is not a rejection here. These candidates only land in the input
/// buffer, and the execution-time guard in `shell::eval` asks before anything
/// runs; discarding them meant paying for a request and showing the user
/// nothing at all.
fn finalize_command<S: AiService + ?Sized>(
    service: &S,
    response: AiCommandResponse,
    what: &str,
) -> Result<String> {
    let mut parts = vec![response.command.clone()];
    parts.extend(response.args.clone());
    let rendered = parts.join(" ");

    let (Some(guard), Some(level), Some(allowlist)) = (
        service.get_safety_guard(),
        service.get_safety_level(),
        service.get_allowlist(),
    ) else {
        // Safety components are unavailable (e.g. in tests).
        return Ok(rendered);
    };

    match guard.check_command(&level, &response.command, &response.args, &allowlist) {
        SafetyResult::Allowed => Ok(rendered),
        SafetyResult::Confirm(msg) => {
            tracing::info!("AI {what} needs confirmation before it runs: {msg}");
            Ok(rendered)
        }
        SafetyResult::Denied(msg) => {
            tracing::warn!("Blocked dangerous AI {what}: {msg}");
            Err(anyhow::anyhow!(
                "AI {what} was blocked by the safety policy: `{}`. {}",
                response.command,
                msg
            ))
        }
    }
}
