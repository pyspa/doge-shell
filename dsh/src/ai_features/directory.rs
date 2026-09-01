//! AI-powered directory analysis.
//!
//! This module provides functions for analyzing directory structure and contents.

use super::cache;
use super::service::{AiRequestOptions, AiService};
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

    if let Some(cached) = cache::lookup("describe_dir", &[&query]) {
        return Ok(cached);
    }

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let answer = service
        .send_request_with(
            messages,
            AiRequestOptions::new(Some(0.3))
                .without_tools()
                .with_prompt_cache_key("dsh-describe-dir"),
        )
        .await?;
    cache::store("describe_dir", &[&query], &answer);
    Ok(answer)
}

/// Files to show the model when it needs to know what is in a directory.
///
/// One implementation, because three used to disagree. The palette's version
/// applied `take(30)` *before* sorting, so on a large directory it sent an
/// arbitrary filesystem-order sample, and it kept dotfiles that only cost
/// tokens. Directories come first, then names, and hidden entries are dropped.
pub const DIRECTORY_LISTING_LIMIT: usize = 30;

pub fn directory_listing_entries(path: &std::path::Path) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(path) else {
        return Vec::new();
    };

    let mut entries: Vec<(String, bool)> = dir
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false);
            (name, is_dir)
        })
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();

    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });

    entries
        .into_iter()
        .take(DIRECTORY_LISTING_LIMIT)
        .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name })
        .collect()
}
