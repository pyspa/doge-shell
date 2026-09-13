//! Character-range math for completion replacement: which slice of the input
//! a chosen candidate replaces, including the special case of an
//! `--opt=value`/`-ovalue` option value embedded in the same token.
use super::*;
use crate::completion::parser;
use crate::completion::shell_token::{self, SeparatorMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionReplacementRange {
    pub start: usize,
    pub end: usize,
}

pub(super) fn completion_replacement_range(
    input: &str,
    cursor_pos: usize,
    parsed_command_line: &ParsedCommandLine,
) -> Option<CompletionReplacementRange> {
    let token_range = token_range_at_cursor(input, cursor_pos)?;
    let token = slice_chars(input, token_range.start, token_range.end);

    if let parser::CompletionContext::OptionValue { option_name, .. } =
        &parsed_command_line.completion_context
        && let Some(value_range) =
            option_value_range_from_token(&token, token_range.start, option_name)
    {
        return Some(value_range);
    }

    Some(token_range)
}

pub(super) fn option_value_range_from_token(
    token: &str,
    token_start: usize,
    option_name: &str,
) -> Option<CompletionReplacementRange> {
    if option_name.starts_with("--") {
        let prefix = format!("{option_name}=");
        if token.starts_with(&prefix) {
            let start = token_start + prefix.chars().count();
            let end = token_start + token.chars().count();
            return Some(CompletionReplacementRange { start, end });
        }
    }

    if option_name.len() == 2 && !option_name.starts_with("--") && token.starts_with(option_name) {
        let value = &token[option_name.len()..];
        if !value.is_empty() && !value.starts_with('=') {
            let start = token_start + option_name.chars().count();
            let end = token_start + token.chars().count();
            return Some(CompletionReplacementRange { start, end });
        }
    }

    None
}

pub(super) fn token_range_at_cursor(
    input: &str,
    cursor_pos: usize,
) -> Option<CompletionReplacementRange> {
    let token =
        shell_token::token_at_char_cursor(input, cursor_pos, SeparatorMode::CompletionRange)?;
    Some(CompletionReplacementRange {
        start: token.char_start,
        end: token.char_end,
    })
}

pub(super) fn slice_chars(input: &str, start: usize, end: usize) -> String {
    input
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

pub(super) fn replace_char_range(
    input: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> String {
    let mut result = String::with_capacity(input.len() + replacement.len());
    for (index, ch) in input.chars().enumerate() {
        if index == start {
            result.push_str(replacement);
        }
        if index < start || index >= end {
            result.push(ch);
        }
    }
    if start >= input.chars().count() {
        result.push_str(replacement);
    }
    result
}

/// User-home (`~user`) candidates, reusing the `/etc/passwd`-backed user
/// generator. `prefix` is the partial user name (the part after `~`); the
/// returned text keeps the leading `~` so it replaces the whole token.
pub(super) fn tilde_user_candidates(prefix: &str) -> Vec<EnhancedCandidate> {
    let generator = crate::completion::generators::user::UserGenerator::new();
    match generator.generate_candidates(prefix) {
        Ok(users) => users
            .into_iter()
            .map(|user| EnhancedCandidate {
                text: format!("~{}", user.text),
                description: user.description,
                candidate_type: CandidateType::Generic,
                priority: 140,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}
