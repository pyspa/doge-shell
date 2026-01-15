//! AI Features module for shell intelligence.
//!
//! This module provides AI-powered features for the shell, including:
//! - Command generation from natural language
//! - Smart pipe expansion
//! - Command fixing
//! - Command explanation and analysis
//! - Directory description
//! - Command suggestions
//!
//! # Module Structure
//!
//! - [`service`] - Core AI service traits and implementations
//! - [`command`] - Command generation and manipulation
//! - [`analysis`] - Command analysis and diagnostics
//! - [`directory`] - Directory structure analysis
//! - [`suggestion`] - Command suggestions and completion generation

mod analysis;
mod command;
mod directory;
mod service;
mod suggestion;

#[cfg(test)]
mod tests;

// Re-export main types and functions for backward compatibility
pub use analysis::{
    analyze_output, check_safety, diagnose_output, explain_command, suggest_improvement,
};
pub use command::{expand_smart_pipe, fix_command, run_generative_command};
pub use directory::describe_directory;
pub use service::{AiCommandResponse, AiService, ChatClient, ConfirmationHandler, LiveAiService};
pub use suggestion::{generate_completion_json, suggest_next_commands};

/// Sanitize code block markers from AI response.
///
/// Removes markdown code block syntax from AI responses.
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
