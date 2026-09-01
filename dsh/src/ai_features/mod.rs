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
mod cache;
mod command;
mod directory;
mod last_failure;
mod service;
mod suggestion;
pub mod ui;

#[cfg(test)]
mod tests;

// Re-export main types and functions for backward compatibility
pub use analysis::{
    check_safety, diagnose_output, diagnose_output_with_history, explain_command,
    explain_command_inline, send_followup_question, suggest_improvement, summarize_watch,
};
pub use command::{expand_smart_pipe, fix_command, run_generative_command};
pub use directory::{describe_directory, directory_listing_entries};
pub use last_failure::{LastFailure, combine_streams, resolve as resolve_last_failure};
pub use service::{
    AiCommandResponse, AiRequestOptions, AiService, ChatClient, ConfirmationHandler, LiveAiService,
};
pub use suggestion::{generate_completion_json, suggest_next_commands};

/// Sanitize code block markers from AI response.
///
/// Removes markdown code block syntax from AI responses.
pub fn sanitize_code_block(content: &str) -> String {
    dsh_openai::strip_code_fence(content)
}
