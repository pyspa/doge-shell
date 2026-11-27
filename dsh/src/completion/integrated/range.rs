//! Character-range math for completion replacement: which slice of the input
//! a chosen candidate replaces, including the special case of an
//! `--opt=value`/`-ovalue` option value embedded in the same token.
use super::*;
use crate::completion::parser;

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
    let mut token_range = token_range_at_cursor(input, cursor_pos)?;
    if parsed_command_line.command == "pacman"
        && matches!(&parsed_command_line.completion_context,
            parser::CompletionContext::OptionValue { option_name, .. }
            if matches!(option_name.as_str(), "--ignore" | "--ignoregroup"))
        && let Some(next_comma) = input
            .chars()
            .skip(cursor_pos)
            .take(token_range.end.saturating_sub(cursor_pos))
            .position(|c| c == ',')
    {
        token_range.end = cursor_pos + next_comma;
    }
    if matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::OptionValue { .. }
    ) {
        // The corrector already identified the raw value before the cursor.
        // Map its length back to the public input, including inside wrappers.
        let cursor = cursor_pos.min(token_range.end);
        let prefix = slice_chars(input, token_range.start, cursor);
        if prefix.ends_with(&parsed_command_line.current_token) {
            token_range.start = cursor - parsed_command_line.current_token.chars().count();
        }
    }
    Some(token_range)
}

pub(crate) fn token_range_at_cursor(
    input: &str,
    cursor_pos: usize,
) -> Option<CompletionReplacementRange> {
    editable_token_range_at_cursor(input, cursor_pos, 0)
}

/// Only edit the current prompt. A quoted word can start on a previous
/// prompt while the reference under the cursor is entirely on this one.
pub(crate) fn editable_token_range_at_cursor(
    input: &str,
    cursor_pos: usize,
    editable_start: usize,
) -> Option<CompletionReplacementRange> {
    let cursor_pos = cursor_pos.min(input.chars().count());
    let cursor_byte = crate::completion::command_segment::char_to_byte(input, cursor_pos);
    let parsed = parser::CommandLineParser::new().parse(input, cursor_byte);
    let source = parsed.source.as_ref()?;
    if source.suppressed {
        return None;
    }
    // Share cursor ownership with provider dispatch, including adjacent redirect
    // operators/targets and zero-width gaps. Never rediscover words by value.
    let word = source.words.get(source.cursor_word)?;
    let offset = input[..cursor_byte.saturating_sub(source.cursor)]
        .chars()
        .count();
    let token_range = CompletionReplacementRange {
        start: offset + word.char_start,
        end: offset + word.char_end,
    };
    if token_range.start >= editable_start {
        return Some(token_range);
    }
    let (_, reference_range) = reference::Reference::at_cursor(&parsed, cursor_pos)?;
    (reference_range.start >= editable_start).then_some(reference_range)
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
