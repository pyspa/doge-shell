//! AI-powered command generation and manipulation.
//!
//! This module provides functions for generating shell commands from natural language,
//! expanding smart pipes, and fixing failed commands.

use super::service::{AiCommandResponse, AiRequestOptions, AiService};
use crate::parser::render::render_literal_command;
use crate::safety::{PromptInjectionResult, SafetyGuard, SafetyResult};
use anyhow::Result;

const LITERAL_ARGV_CONTRACT: &str = "Return exactly two required fields: command (string) and args (array of strings); no other fields. \
The command field is one executable name, never a shell source string. \
Each args element is one literal argv value, including spaces or an empty string. \
Do not add shell quoting or escaping: JSON escaping only. \
Globs, dollar expressions, pipes, redirects and separators in values are literal data, not shell syntax. \
Do not put a compound shell program in command or args. NUL bytes are invalid.";

fn command_json_options() -> AiRequestOptions {
    // No token cap: on a reasoning model `max_completion_tokens` also budgets
    // hidden reasoning, so a small cap yields finish_reason=length and no JSON.
    //
    // No tools either: these three write a command line from what the user
    // already typed. Offering the MCP schemas here only bought the chance of a
    // tool call the caller has no way to confirm.
    AiRequestOptions::new(Some(0.1))
        .as_json_object()
        .without_tools()
        .with_prompt_cache_key("dsh-command-json")
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

    let system_prompt = format!(
        "You are a shell command expert. The user wants to extend a shell pipeline. \
    Target platform: {}. \
    Given the user's natural language query, output the next command in the pipeline as a JSON object. \
    Format: {{\"command\": \"grep\", \"args\": [\"-r\", \"pattern\"]}}. \
    {} Return one pipeline stage, never pipeline source. Do not output markdown code blocks. Output ONLY the JSON object.",
        target_platform_description(),
        LITERAL_ARGV_CONTRACT
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": sanitized_query}),
    ];

    let content = service
        .send_request_with(messages, command_json_options())
        .await?;
    let response = parse_command_response(&content)?;

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

    // Naming the wrong platform is worse than naming none: on macOS a Linux
    // prompt produces GNU-only flags (`sed -i` without an argument, `ls
    // --color`) that fail on the BSD tools actually installed.
    let system_prompt = format!(
        "You are a shell command expert. Convert the following natural language request into a single executable invocation with literal arguments. \
    Target platform: {}. \
    Output the result as a JSON object. \
    Format: {{\"command\": \"rm\", \"args\": [\"-rf\", \"/\"]}}. \
    {} Do not output markdown code blocks. Output ONLY the JSON object.",
        target_platform_description(),
        LITERAL_ARGV_CONTRACT
    );

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": sanitized_query}),
    ];

    let content = service
        .send_request_with(messages, command_json_options())
        .await?;
    let response = parse_command_response(&content)?;

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

    let system_prompt = format!(
        "You are a shell command expert. The user executed a command that failed. \
    Target platform: {}. \
    Given the failed command, its exit code, and its output (including potential error messages), \
    output the corrected command as a JSON object. \
    Format: {{\"command\": \"grep\", \"args\": [\"-r\", \"pattern\"]}}. \
    {} Do not output markdown code blocks. Output ONLY the JSON object. \
    If you cannot determine a fix, preserve the original executable and literal argv in this schema; never put the entire original shell source into command.",
        target_platform_description(),
        LITERAL_ARGV_CONTRACT
    );

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
    let response = parse_command_response(&content)?;

    finalize_command(service, response, "fix suggestion")
}

/// Accept exactly one typed JSON object, with a complete legacy JSON fence.
/// Do not repair malformed envelopes or parse through Value: that loses duplicate
/// fields before Serde can reject them. Error text must not echo provider values
/// or field names (Serde's detailed data errors can contain either).
fn parse_command_response(content: &str) -> Result<AiCommandResponse> {
    let content = content.trim_matches([' ', '\n', '\r', '\t']);
    let payload = if content.starts_with("```") {
        let (header, body) = content
            .split_once('\n')
            .ok_or_else(|| anyhow::anyhow!("Invalid AI command JSON: incomplete JSON fence"))?;
        if !matches!(header.trim_end_matches('\r'), "```json" | "```") {
            anyhow::bail!("Invalid AI command JSON: unsupported fence");
        }
        body.strip_suffix("\n```")
            .ok_or_else(|| anyhow::anyhow!("Invalid AI command JSON: incomplete JSON fence"))?
    } else {
        content
    };
    if !payload
        .trim_start_matches([' ', '\n', '\r', '\t'])
        .starts_with('{')
    {
        anyhow::bail!("Invalid AI command JSON: expected a JSON object");
    }
    serde_json::from_str(payload).map_err(|error| {
        anyhow::anyhow!(
            "Invalid AI command JSON: {:?} at line {}, column {}; expected only command (string) and args (array of strings)",
            error.classify(), error.line(), error.column()
        )
    })
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
    let rendered = render_literal_command(&response.command, &response.args)?;

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
    }
}

/// Describe the host so generated commands use tools that exist here.
///
/// doge-shell supports Linux and macOS, and their coreutils differ enough that
/// a command written for one regularly fails on the other.
pub(crate) fn target_platform_description() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macOS with BSD coreutils, POSIX sh/zsh"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Linux with GNU coreutils, POSIX sh/bash"
    }
}
