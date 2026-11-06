//! Completion's tolerant source view. Execution syntax is never evaluated here:
//! lexical words keep nested syntax opaque and only outer control operators
//! split invocations. Coordinates are bytes until the editor replacement edge.
use super::shell_token::ShellTokenSpan;
use crate::parser::{Rule, ShellParser};
use pest::Parser;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct CommandSource {
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) words: Vec<ShellTokenSpan>,
    pub(crate) cursor_word: usize,
    /// Index of argv0 (or the empty command gap), distinct from prefix words.
    pub(crate) command_word: usize,
    pub(crate) argument_start: usize,
    pub(crate) multiple: bool,
    pub(crate) suppressed: bool,
}

impl CommandSource {
    pub(crate) fn raw_index(&self, raw_len: usize) -> Option<usize> {
        self.cursor_word
            .checked_sub(self.words.len().saturating_sub(raw_len))
    }
}

pub(crate) struct CommandSegment {
    pub(crate) range: Range<usize>,
    pub(crate) multiple: bool,
    pub(crate) suppressed: bool,
}

pub(crate) fn char_to_byte(input: &str, cursor: usize) -> usize {
    input
        .char_indices()
        .nth(cursor)
        .map_or(input.len(), |(i, _)| i)
}

pub(crate) fn floor_byte(input: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(input.len());
    while !input.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

pub(crate) fn word_span(input: &str, range: Range<usize>) -> ShellTokenSpan {
    ShellTokenSpan {
        raw: input[range.clone()].to_string(),
        byte_start: range.start,
        byte_end: range.end,
        char_start: input[..range.start].chars().count(),
        char_end: input[..range.end].chars().count(),
    }
}

pub(crate) fn words(input: &str) -> Vec<ShellTokenSpan> {
    let Ok(mut pairs) = ShellParser::parse(Rule::completion_scan, input) else {
        return Vec::new();
    };
    pairs
        .next()
        .unwrap()
        .into_inner()
        .filter(|p| {
            matches!(
                p.as_rule(),
                Rule::completion_word | Rule::completion_redirect
            )
        })
        .map(|p| word_span(input, p.as_span().start()..p.as_span().end()))
        .collect()
}

pub(crate) fn command_segment_at_cursor(input: &str, cursor: usize) -> CommandSegment {
    let cursor = floor_byte(input, cursor);
    let mut result = CommandSegment {
        range: 0..input.len(),
        multiple: false,
        suppressed: false,
    };
    let Ok(mut pairs) = ShellParser::parse(Rule::completion_scan, input) else {
        result.suppressed = true;
        return result;
    };
    let mut has_word = false;
    let mut special_start = None;
    for pair in pairs.next().unwrap().into_inner() {
        let span = pair.as_span();
        match pair.as_rule() {
            Rule::completion_word => {
                // A capture suffix has no following command without a separator.
                if special_start.is_some() {
                    result.suppressed = true;
                }
                has_word = true;
            }
            Rule::completion_redirect => {}
            Rule::completion_unsupported => result.suppressed = true,
            Rule::comment => {
                if cursor >= span.start() && cursor <= span.end() {
                    result.suppressed = true;
                }
            }
            Rule::completion_struct | Rule::completion_capture => {
                if cursor >= span.start() && cursor <= span.end() {
                    result.suppressed = true;
                }
                special_start = Some(span.start());
            }
            Rule::completion_control | Rule::completion_newline => {
                let newline = pair.as_rule() == Rule::completion_newline;
                // A newline after &&/||/| continues the pending RHS. Blank lines
                // are legal; consecutive control operators are not.
                if !has_word && !newline {
                    result.suppressed = true;
                }
                if special_start.is_some_and(|start| start <= cursor && cursor <= span.start()) {
                    result.suppressed = true;
                }
                result.multiple = true;
                if span.start() < cursor && cursor < span.end() {
                    result.suppressed = true;
                }
                if span.end() <= cursor {
                    result.range.start = span.end();
                } else if span.start() >= cursor && result.range.end == input.len() {
                    result.range.end = span.start();
                }
                has_word = false;
                special_start = None;
            }
            _ => {}
        }
    }
    if special_start.is_some_and(|start| cursor >= start) {
        result.suppressed = true;
    }
    result
}

pub(crate) fn smart_pipe_marker(input: &str) -> Option<usize> {
    let pairs = ShellParser::parse(Rule::completion_scan, input)
        .ok()?
        .next()?;
    for pair in pairs.into_inner() {
        let start = pair.as_span().start();
        if pair.as_rule() == Rule::completion_control
            && pair.as_str() == "|"
            && input[start..].starts_with("|?")
            && !command_segment_at_cursor(&input[..start], start).suppressed
        {
            return Some(start);
        }
    }
    None
}

#[cfg(test)]
mod tests;
