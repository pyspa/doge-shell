//! AI-powered directory analysis.
//!
//! This module provides functions for analyzing directory structure and contents.

use super::service::AiService;
use crate::safety::SafetyGuard;
use anyhow::Result;
use serde_json::json;

/// Describe the current directory structure.
///
/// Analyzes the directory listing and identifies the project type,
/// technology stack, and suggests relevant commands.
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
