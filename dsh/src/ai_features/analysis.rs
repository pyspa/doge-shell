//! AI-powered command analysis and diagnostics.
//!
//! This module provides functions for explaining commands, checking safety,
//! suggesting improvements, and diagnosing command output.

use super::cache;
use super::service::{AiRequestOptions, AiService};
use crate::safety::{PromptInjectionResult, SafetyGuard};
use anyhow::Result;
use serde_json::json;

/// Bound captured output without throwing away the end of it.
///
/// `sanitize_ai_input` truncates from the head, and a failing build explains
/// itself in its *last* lines - so the diagnosis prompts were being handed the
/// compiler's progress messages with the error cut off. Trimming the middle
/// first keeps both ends; the sanitiser then only has the control characters
/// and the invisible ones left to do.
fn bounded_output(output: &str, budget: usize) -> String {
    let bounded = dsh_openai::turn::truncate_middle(output, budget);
    // Headroom for the marker `truncate_middle` inserts, so the sanitiser does
    // not immediately cut the head off again.
    SafetyGuard::sanitize_ai_input(&bounded, budget + 256)
}

/// Cap for the single-line inline explanation.
const INLINE_ANSWER_TOKENS: u64 = 120;

/// Options for a request that only reasons about text it was handed.
///
/// No tools - these features have nothing to look up, and attaching the MCP
/// schemas made a 140-token question cost thousands. A cache key per feature,
/// so the provider can reuse each one's stable system prompt.
fn read_only_options(
    temperature: f64,
    cache_key: &str,
    max_tokens: Option<u64>,
) -> AiRequestOptions {
    AiRequestOptions::new(Some(temperature))
        .without_tools()
        .with_prompt_cache_key(&format!("dsh-{cache_key}"))
        .with_max_tokens(max_tokens)
}

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

    // Explaining the same command twice is the same answer.
    if let Some(cached) = cache::lookup("explain", &[&sanitized_command]) {
        return Ok(cached);
    }

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Explain this command:\n```\n{}\n```", sanitized_command)}),
    ];

    let answer = service
        .send_request_with(messages, read_only_options(0.2, "explain", None))
        .await?;
    cache::store("explain", &[&sanitized_command], &answer);
    Ok(answer)
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

    // The prompt asks for a single short line; cap the generation to match.
    if let Some(cached) = cache::lookup("explain_inline", &[&sanitized_command]) {
        return Ok(cached);
    }

    let answer = service
        // One line of output, so a cap here cannot cut anything the caller uses.
        .send_request_with(
            messages,
            read_only_options(0.1, "explain-inline", Some(INLINE_ANSWER_TOKENS)),
        )
        .await?;
    cache::store("explain_inline", &[&sanitized_command], &answer);
    Ok(answer)
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

    service
        .send_request_with(
            messages,
            read_only_options(0.3, "suggest-improvement", None),
        )
        .await
}

/// Check if a command is potentially dangerous.
pub async fn check_safety<S: AiService + ?Sized>(service: &S, command: &str) -> Result<String> {
    // Does not need heavy sanitization as it is security check itself, but prevent injection
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 2000);

    let system_prompt = "You are a security-conscious shell expert. Analyze the given command for potential security risks. \
    If the command is dangerous (e.g., deletes files, modifies system settings, sends data externally), explain the risk. \
    Output 'SAFE' if the command appears safe. \
    Output 'WARNING: <reason>' if there are risks.";

    if let Some(cached) = cache::lookup("check_safety", &[&sanitized_command]) {
        return Ok(cached);
    }

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Check safety of:\n```\n{}\n```", sanitized_command)}),
    ];

    let answer = service
        .send_request_with(messages, read_only_options(0.1, "check-safety", None))
        .await?;
    cache::store("check_safety", &[&sanitized_command], &answer);
    Ok(answer)
}

/// Diagnose command output (especially errors).
pub async fn diagnose_output<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    exit_code: i32,
) -> Result<String> {
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    // Bounded below by `bounded_output`, which keeps the tail; this pass is
    // here for the control characters, not for the length.
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 64_000);

    let system_prompt = "You are a debugging expert. Analyze the command output and diagnose any issues. \
    Focus on error messages and their root causes. Provide clear, actionable solutions. \
    Respond in the same language as the user's environment if possible, or match the language of their request.";

    // Truncate output if too long
    let truncated_output = bounded_output(&sanitized_output, 4000);

    let query = format!(
        "Command: `{}`\nExit code: {}\nOutput:\n```\n{}\n```",
        sanitized_command, exit_code, truncated_output
    );

    if let Some(cached) = cache::lookup("diagnose", &[&query]) {
        return Ok(cached);
    }

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let answer = service
        .send_request_with(messages, read_only_options(0.2, "diagnose", None))
        .await?;
    cache::store("diagnose", &[&query], &answer);
    Ok(answer)
}

/// Diagnose command output and return both the response and the conversation history.
pub async fn diagnose_output_with_history<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    output: &str,
    exit_code: i32,
) -> Result<(String, Vec<serde_json::Value>)> {
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    // Bounded below by `bounded_output`, which keeps the tail; this pass is
    // here for the control characters, not for the length.
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 64_000);

    let system_prompt = "You are a debugging expert. Analyze the command output and diagnose any issues. \
    Focus on error messages and their root causes. Provide clear, actionable solutions. \
    Respond in the same language as the user's environment if possible, or match the language of their request. \
    Additionally, output markdown bash code blocks (```bash ... ```) when proposing commands, \
    so the user can easily copy or apply them.";

    // Truncate output if too long
    let truncated_output = bounded_output(&sanitized_output, 4000);

    let query = format!(
        "Command: `{}`\nExit code: {}\nOutput:\n```\n{}\n```",
        sanitized_command, exit_code, truncated_output
    );

    let mut messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let response = service
        .send_request_with(
            messages.clone(),
            read_only_options(0.2, "diagnose-conversation", None),
        )
        .await?;
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

    let response = service
        .send_request_with(
            history.clone(),
            read_only_options(0.2, "diagnose-conversation", None),
        )
        .await?;
    history.push(json!({"role": "assistant", "content": response.clone()}));

    Ok(response)
}

/// Summarize a watched command's output for the `ai-watch` block record.
pub async fn summarize_watch<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    goal: Option<&str>,
    output: &str,
    exit_code: i32,
    duration_ms: u64,
) -> Result<String> {
    let sanitized_command = SafetyGuard::sanitize_ai_input(command, 1000);
    let sanitized_goal = goal.map(|goal| SafetyGuard::sanitize_ai_input(goal, 1000));
    let sanitized_output = SafetyGuard::sanitize_ai_input(output, 40000);
    let truncated_output = bounded_output(&sanitized_output, 5000);

    let system_prompt = "You are an ai-watch assistant embedded in a shell. \
    Summarize the watched command execution concisely. \
    Include: status, key evidence from output, and next action if useful. \
    Do not claim to have executed anything. Do not propose destructive commands unless clearly necessary. \
    If you suggest commands, put them in a bash code block. Respond in the user's language when possible.";

    let query = format!(
        "Command: `{}`\nGoal: {}\nExit code: {}\nDuration: {} ms\nOutput:\n```\n{}\n```",
        sanitized_command,
        sanitized_goal.as_deref().unwrap_or("(none)"),
        exit_code,
        duration_ms,
        truncated_output
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    service
        .send_request_with(messages, read_only_options(0.2, "watch-summary", None))
        .await
}
