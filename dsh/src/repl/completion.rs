use crate::completion::path::path_completion_prefix_for_shell_token;
use crate::completion::shell_token::{self, SeparatorMode};
use crate::environment::Environment;
use crate::input::Input;
use crate::parser::Rule;
use crate::suggestion::{SuggestionSource, SuggestionState};
use parking_lot::RwLock;
use std::sync::Arc;

const MCP_FORM_SUGGESTIONS: &[&str] =
    &["mcp-add-stdio", "mcp-add-http", "mcp-add-sse", "mcp-clear"];
#[cfg(test)]
mod completion_tests;

pub fn completion_suggestion(
    input_state: &Input,
    input: &str,
    environment: &Arc<RwLock<Environment>>,
) -> Option<SuggestionState> {
    if input.is_empty() || input_state.cursor() != input_state.len() {
        return None;
    }

    if let Ok(words) = input_state.get_words()
        && let Some(full) = word_based_completion(environment, input, &words)
    {
        return Some(SuggestionState {
            full,
            source: SuggestionSource::Completion,
        });
    }

    mcp_form_completion(input).map(|full| SuggestionState {
        full,
        source: SuggestionSource::Completion,
    })
}

pub fn word_based_completion(
    environment: &Arc<RwLock<Environment>>,
    input: &str,
    words: &[(Rule, pest::Span<'_>, bool)],
) -> Option<String> {
    for (rule, span, current) in words {
        if !*current {
            continue;
        }
        let word = span.as_str();
        if word.is_empty() {
            continue;
        }
        match rule {
            Rule::argv0 => {
                if let Some(result) = complete_command_word(environment, input, span, word) {
                    return Some(result);
                }
            }
            Rule::args => {
                if let Some(result) = complete_argument_word(input, span, word) {
                    return Some(result);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn complete_command_word(
    environment: &Arc<RwLock<Environment>>,
    input: &str,
    span: &pest::Span<'_>,
    word: &str,
) -> Option<String> {
    let candidate = {
        let env = environment.read();
        env.search_prefix(word)
    };

    if let Some(name) = candidate
        && name.len() > word.len()
    {
        return Some(replace_range(input, span.start(), span.end(), &name));
    }

    complete_path_for_span(input, span.start(), span.end(), true).map(|completion| completion.full)
}

pub fn complete_argument_word(input: &str, span: &pest::Span<'_>, _word: &str) -> Option<String> {
    complete_path_for_span(input, span.start(), span.end(), false).map(|completion| completion.full)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PathCompletion {
    pub(super) full: String,
    pub(super) suffix: String,
}

pub(super) fn complete_path_for_span(
    input: &str,
    span_start: usize,
    span_end: usize,
    only_dirs: bool,
) -> Option<PathCompletion> {
    let token = shell_token_for_byte_span(input, span_start, span_end)?;
    let candidate = path_completion_prefix_for_shell_token(&token.raw, only_dirs)
        .ok()
        .flatten()?;
    let suffix = candidate.strip_prefix(&token.raw)?;
    if suffix.is_empty() {
        return None;
    }

    Some(PathCompletion {
        full: replace_range(input, token.byte_start, token.byte_end, &candidate),
        suffix: suffix.to_string(),
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
        .map(|idx| idx + 1)
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
