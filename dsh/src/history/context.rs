//! Context detection for history entries.
//!
//! Provides functions to determine the current context (e.g., git repository root)
//! for context-aware history features.

use std::process::Command;

/// Get the current context for history entries.
///
/// Tries to determine the git repository root first, then falls back to
/// the current working directory.
pub fn get_current_context() -> Option<String> {
    // Try to get git root
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() {
            return Some(root);
        }
    }

    // Fallback to current directory
    std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}
