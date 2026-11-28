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
    /// Arguments of this invocation with their source indexes, excluding shell redirections.
    pub(crate) fn argv(&self) -> impl Iterator<Item = (usize, &str)> {
        let mut redirect_target = false;
        self.words
            .iter()
            .enumerate()
            .skip(self.command_word + 1)
            .map(|(index, word)| (index, word.raw.as_str()))
            .filter(move |(_, word)| {
                if redirect_target {
                    redirect_target = false;
                    return false;
                }
                redirect_target = super::parser::CommandLineParser::is_redirect_operator(word);
                !redirect_target
            })
    }

    pub(crate) fn cursor_follows_redirect(&self) -> bool {
        self.cursor_word
            .checked_sub(1)
            .and_then(|index| self.words.get(index))
            .is_some_and(|word| super::parser::CommandLineParser::is_redirect_operator(&word.raw))
    }

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
    use crate::parser::check::{SHELL_WHITESPACE, after_leading_continuations};

    let cursor = floor_byte(input, cursor);
    // A line-head pipe reads the previous output. Scan its downstream command
    // in its own coordinates, then map the active segment back to the editor.
    let head = after_leading_continuations(input).trim_start_matches(SHELL_WHITESPACE);
    let Some(tail) = head.strip_prefix('|') else {
        return scan_segment(input, cursor);
    };
    let pipe = input.len() - head.len();
    let start = if tail
        .trim_start_matches("\\\n")
        .starts_with(['|', '>', ':', '&'])
    {
        pipe
    } else {
        input.len() - after_leading_continuations(tail).len()
    };
    if cursor < start {
        // The only completable sites in the skipped prefix are gaps after
        // the pipe. Inserting between a backslash and LF breaks the splice.
        let inside_splice = input[..cursor].ends_with('\\') && input[cursor..].starts_with('\n');
        return CommandSegment {
            range: cursor..cursor,
            multiple: true,
            suppressed: cursor <= pipe || inside_splice,
        };
    }
    let mut segment = scan_segment(&input[start..], cursor - start);
    segment.range = start + segment.range.start..start + segment.range.end;
    segment.multiple = true;
    segment
}

fn scan_segment(input: &str, cursor: usize) -> CommandSegment {
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
            Rule::line_continuation => {
                // Inserting between the backslash and LF would break the splice.
                if span.start() < cursor && cursor < span.end() {
                    result.suppressed = true;
                }
            }
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
                } else if span.start() >= cursor {
                    result.range.end = span.start();
                    // Commands beyond this boundary cannot change the cursor's context.
                    break;
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

/// Tab and Enter recognize different input shortcuts.
pub(crate) enum PipeTrigger {
    Tab,
    Enter,
}

/// Tab considers both markers because the first prompt owns its remaining
/// text. Enter independently looks for its output-analysis marker.
pub(crate) fn top_level_pipe_marker(input: &str, trigger: PipeTrigger) -> Option<usize> {
    let markers: &[&str] = match trigger {
        PipeTrigger::Tab => &["|?", "|!"],
        PipeTrigger::Enter => &["|!"],
    };
    if !markers.iter().any(|marker| input.contains(marker)) {
        return None;
    }
    let pairs = ShellParser::parse(Rule::completion_scan, input)
        .ok()?
        .next()?;
    for pair in pairs.into_inner() {
        let start = pair.as_span().start();
        if pair.as_rule() == Rule::completion_control
            && pair.as_str() == "|"
            && markers
                .iter()
                .any(|marker| input[start..].starts_with(marker))
        {
            let prefix = &input[..start];
            let segment = match trigger {
                PipeTrigger::Tab => {
                    let segment = command_segment_at_cursor(prefix, start);
                    if segment.multiple
                        && prefix[segment.range.clone()]
                            .trim_matches(crate::parser::check::SHELL_WHITESPACE)
                            .is_empty()
                    {
                        continue;
                    }
                    segment
                }
                // Output analysis runs the left side in an isolated shell.
                // Only interactive completion can use the previous output.
                PipeTrigger::Enter => scan_segment(prefix, start),
            };
            if !segment.suppressed {
                return Some(start);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
