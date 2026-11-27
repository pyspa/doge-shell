use crate::completion::generators::filesystem::FileSystemContext;
use crate::completion::path::path_completion_prefix_for_shell_token;
use crate::completion::shell_path::{decode_literal_word, escape_unquoted};
use crate::completion::shell_token::{self, SeparatorMode};
use crate::environment::Environment;
use crate::input::Input;
use crate::parser::{self, Rule, ShellParser};
use crate::suggestion::{SuggestionSource, SuggestionState};
use parking_lot::RwLock;
use pest::Parser;
use pest::iterators::{Pair, Pairs};
use std::sync::Arc;

const MCP_FORM_SUGGESTIONS: &[&str] =
    &["mcp-add-stdio", "mcp-add-http", "mcp-add-sse", "mcp-clear"];
#[cfg(test)]
mod tests;

pub(super) fn completion_suggestion(
    input_state: &Input,
    input: &str,
    environment: &Arc<RwLock<Environment>>,
    filesystem: &FileSystemContext,
    only_dirs: bool,
) -> Option<SuggestionState> {
    if input.is_empty() || input_state.cursor() != input_state.len() {
        return None;
    }

    if let Ok(pairs) = ShellParser::parse(Rule::commands, input)
        && let Some(completion) =
            word_based_completion(environment, input, pairs, filesystem, only_dirs)
    {
        return Some(SuggestionState {
            full: completion.full,
            source: SuggestionSource::Completion,
        });
    }

    if only_dirs {
        return None;
    }
    mcp_form_completion(input).map(|full| SuggestionState {
        full,
        source: SuggestionSource::Completion,
    })
}

pub(super) fn word_based_completion(
    environment: &Arc<RwLock<Environment>>,
    input: &str,
    pairs: Pairs<'_, Rule>,
    filesystem: &FileSystemContext,
    only_dirs: bool,
) -> Option<WordCompletion> {
    if let Some(word) = pairs
        .clone()
        .next()
        .and_then(|pair| trailing_literal_command_word(pair, input.len()))
    {
        let prefix = decode_literal_word(word);
        if let Some(name) = environment.read().search_prefix(&prefix) {
            // The PATH cache holds filenames, which need encoding before they
            // become shell source. Control characters also affect the terminal.
            if name.chars().any(char::is_control) {
                return None;
            }
            let remaining = name
                .strip_prefix(&prefix)
                .filter(|suffix| !suffix.is_empty())?;
            // Keep the typed spelling. In particular, a bare '#' inside
            // an existing word is already literal.
            let suffix = escape_unquoted(remaining);
            return Some(WordCompletion {
                full: format!("{input}{suffix}"),
            });
        }
    }
    parser::get_words_from_pairs(pairs, input.len())
        .into_iter()
        .filter(|(_, span, current)| *current && !span.as_str().is_empty())
        .find_map(|(rule, span, _)| match rule {
            Rule::argv0 | Rule::args => complete_path_for_span(
                input,
                span.start(),
                span.end(),
                only_dirs || rule == Rule::argv0,
                filesystem,
            ),
            _ => None,
        })
}

fn trailing_literal_command_word(mut pair: Pair<'_, Rule>, end: usize) -> Option<&str> {
    while pair.as_span().end() == end {
        match pair.as_rule() {
            Rule::commands
            | Rule::and_or_list
            | Rule::command
            | Rule::pipe_command
            | Rule::simple_command => {
                pair = pair.into_inner().next_back()?;
            }
            Rule::argv0 => {
                // Match the complete argv0, including any adjacent parts.
                // A leading unquoted tilde belongs to path expansion.
                let mut parts = pair.into_inner().next()?.into_inner();
                let word = parts.next()?;
                let raw = word.as_str();
                return (word.as_rule() == Rule::word
                    && parts.next().is_none()
                    && !raw.starts_with('~'))
                .then_some(raw);
            }
            _ => break,
        }
    }
    None
}

/// Legacy path-only entry point. The interactive REPL supplies its resolved
/// request context to `complete_argument_word_in_context` instead.
pub fn complete_argument_word(
    environment: &Arc<RwLock<Environment>>,
    input: &str,
    span: &pest::Span<'_>,
    word: &str,
) -> Option<String> {
    let filesystem = FileSystemContext::new(
        &std::env::current_dir().ok()?,
        environment.read().lookup_variable("HOME"),
    );
    complete_argument_word_in_context(&filesystem, input, span, word)
}

fn complete_argument_word_in_context(
    filesystem: &FileSystemContext,
    input: &str,
    span: &pest::Span<'_>,
    _word: &str,
) -> Option<String> {
    complete_path_for_span(input, span.start(), span.end(), false, filesystem)
        .map(|completion| completion.full)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WordCompletion {
    pub(super) full: String,
}

pub(super) fn complete_path_for_span(
    input: &str,
    span_start: usize,
    span_end: usize,
    only_dirs: bool,
    filesystem: &FileSystemContext,
) -> Option<WordCompletion> {
    let token = shell_token_for_byte_span(input, span_start, span_end)?;
    let candidate = path_completion_prefix_for_shell_token(&token.raw, only_dirs, Some(filesystem))
        .ok()
        .flatten()?;
    let suffix = candidate.strip_prefix(&token.raw)?;
    if suffix.is_empty() {
        return None;
    }

    Some(WordCompletion {
        full: replace_range(input, token.byte_start, token.byte_end, &candidate),
    })
}

fn shell_token_for_byte_span(
    input: &str,
    span_start: usize,
    span_end: usize,
) -> Option<shell_token::ShellTokenSpan> {
    shell_token::tokenize(input, SeparatorMode::CompletionRange)
        .into_iter()
        .find(|token| token.byte_start <= span_start && span_end <= token.byte_end)
}

pub fn mcp_form_completion(input: &str) -> Option<String> {
    let trimmed = input.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let token = trailing_symbol(trimmed);
    if token.is_empty() || !token.starts_with("mcp-") {
        return None;
    }
    for candidate in MCP_FORM_SUGGESTIONS {
        if candidate.starts_with(token) && candidate.len() > token.len() {
            let suffix = &candidate[token.len()..];
            let mut output = trimmed.to_string();
            output.push_str(suffix);
            if trimmed.len() < input.len() {
                output.push_str(&input[trimmed.len()..]);
            }
            return Some(output);
        }
    }
    None
}

pub fn trailing_symbol(input: &str) -> &str {
    let boundary = input
        .rfind(|c: char| c.is_whitespace() || matches!(c, '(' | ')'))
        .map(|idx| idx + input[idx..].chars().next().unwrap().len_utf8())
        .unwrap_or(0);
    &input[boundary..]
}

pub fn replace_range(input: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut result = String::with_capacity(input.len() + replacement.len());
    result.push_str(&input[..start]);
    result.push_str(replacement);
    result.push_str(&input[end..]);
    result
}

pub fn next_word_chunk(suffix: &str) -> Option<String> {
    if suffix.is_empty() {
        return None;
    }

    let mut end = suffix.len();
    let mut in_word = false;
    for (idx, ch) in suffix.char_indices() {
        if ch.is_whitespace() {
            if in_word {
                end = idx + ch.len_utf8();
                break;
            }
        } else {
            in_word = true;
        }
    }

    if !in_word {
        return Some(suffix.to_string());
    }

    Some(suffix[..end.min(suffix.len())].to_string())
}
