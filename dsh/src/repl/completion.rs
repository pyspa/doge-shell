use crate::completion::generators::filesystem::{
    FileSystemContext, file_name_matches_extension_filters, normalize_extension_filters,
};
use crate::completion::integrated::GenericCompletion;
use crate::completion::path::path_completion_prefix_for_shell_token_filtered;
use crate::completion::shell_path::{decode_literal_word, escape_unquoted};
use crate::completion::shell_token::{self, SeparatorMode};
use crate::environment::Environment;
use crate::input::Input;
use crate::parser::{Rule, ShellParser};
use crate::suggestion::{SuggestionSource, SuggestionState};
use parking_lot::RwLock;
use pest::Parser;
use pest::iterators::Pair;
use std::sync::Arc;

const MCP_FORM_SUGGESTIONS: &[&str] =
    &["mcp-add-stdio", "mcp-add-http", "mcp-add-sse", "mcp-clear"];
#[cfg(test)]
mod tests;

pub(super) fn automatic_shell_region(input: &str) -> bool {
    use crate::ai_task_input::{TabAction, tab_action};
    let cursor = input.chars().count();
    matches!(tab_action(input, cursor), TabAction::Shell)
        && crate::completion::integrated::editable_token_range_at_cursor(input, cursor, 0).is_some()
}

pub(super) fn session_command_suggestion(
    shell: &crate::shell::Shell,
    input: &str,
    token: &str,
) -> Option<String> {
    use skim::SkimItem;

    let word = literal_command_word(token)?;
    let prefix = decode_literal_word(word);
    if prefix.contains('/') || prefix.is_empty() {
        return None;
    }
    let names =
        crate::completion::session_commands::session_command_names(shell, word == prefix, |name| {
            name.len() > prefix.len()
                && name.starts_with(&prefix)
                && !name.chars().any(char::is_control)
        });
    let name = names.iter().map(|candidate| candidate.output()).min()?;
    Some(format!("{input}{}", escape_unquoted(&name[prefix.len()..])))
}

pub(super) fn completion_suggestion(
    input_state: &Input,
    input: &str,
    token: &str,
    environment: &Arc<RwLock<Environment>>,
    filesystem: &FileSystemContext,
    policy: &GenericCompletion,
) -> Option<SuggestionState> {
    if input.is_empty()
        || input_state.cursor() != input_state.len()
        || matches!(
            policy,
            GenericCompletion::DeclaredValues | GenericCompletion::OptionName
        )
    {
        return None;
    }
    let mut full = None;
    if matches!(
        policy,
        GenericCompletion::Command | GenericCompletion::ExternalCommand
    ) && let Some(word) = literal_command_word(token)
    {
        full = path_command_suggestion(environment, input, word).map(|completion| completion.full);
    }
    let full = full
        .or_else(|| {
            let prefix = input.strip_suffix(token)?;
            let candidate = path_candidate(token, policy, filesystem)?;
            Some(format!("{prefix}{candidate}"))
        })
        .or_else(|| mcp_form_completion(input))?;
    Some(SuggestionState {
        full,
        source: SuggestionSource::Completion,
    })
}

fn path_command_suggestion(
    environment: &Arc<RwLock<Environment>>,
    input: &str,
    raw_prefix: &str,
) -> Option<WordCompletion> {
    let prefix = decode_literal_word(raw_prefix);
    if prefix.contains('/') || prefix.starts_with('~') || prefix.contains('$') {
        return None;
    }
    let name = environment.read().search_prefix(&prefix)?;
    if name.chars().any(char::is_control) {
        return None;
    }
    let remaining = name
        .strip_prefix(&prefix)
        .filter(|suffix| !suffix.is_empty())?;
    // Keep the typed spelling, including a literal '#' within the word.
    Some(WordCompletion {
        full: format!("{input}{}", escape_unquoted(remaining)),
    })
}

fn literal_command_word(token: &str) -> Option<&str> {
    let pair = ShellParser::parse(Rule::commands, token).ok()?.next()?;
    trailing_literal_command_word(pair, token.len())
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
    let policy = if only_dirs {
        GenericCompletion::Directory
    } else {
        GenericCompletion::Files
    };
    let candidate = path_candidate(&token.raw, &policy, filesystem)?;
    Some(WordCompletion {
        full: replace_range(input, token.byte_start, token.byte_end, &candidate),
    })
}

fn path_candidate(
    token: &str,
    policy: &GenericCompletion,
    filesystem: &FileSystemContext,
) -> Option<String> {
    let explicit = decode_literal_word(token).contains('/');
    let home = token.starts_with("~/");
    let extensions = match policy {
        GenericCompletion::FileExtensions(values) => Some(normalize_extension_filters(values)),
        _ => None,
    };
    let candidate =
        path_completion_prefix_for_shell_token_filtered(token, Some(filesystem), |path| {
            if path.ends_with('/') {
                return *policy != GenericCompletion::ExternalCommand || explicit;
            }
            match policy {
                GenericCompletion::Directory => false,
                GenericCompletion::Command | GenericCompletion::ExternalCommand => {
                    explicit
                        && crate::environment::is_executable_file(
                            &filesystem.lookup_directory(path, home),
                        )
                }
                _ => extensions
                    .as_deref()
                    .is_none_or(|filters| file_name_matches_extension_filters(path, filters)),
            }
        })
        .ok()
        .flatten()?;
    candidate
        .strip_prefix(token)
        .filter(|suffix| !suffix.is_empty())?;
    Some(candidate)
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
    let head = trimmed.trim_start_matches(crate::parser::check::SHELL_WHITESPACE);
    let token = head.strip_prefix('(').map_or(head, |form| {
        form.trim_start_matches(crate::parser::check::SHELL_WHITESPACE)
    });
    if !token.starts_with("mcp-") {
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

pub(crate) fn next_word_chunk(prefix: &str, suffix: &str) -> Option<String> {
    use crate::parser::check::SHELL_WHITESPACE;

    if suffix.is_empty() {
        return None;
    }

    let full = format!("{prefix}{suffix}");
    let parsed = ShellParser::parse(Rule::completion_scan, &full)
        .ok()?
        .next()?;
    let boundary = prefix.len();
    let Some(item) = parsed.into_inner().find(|item| {
        item.as_span().end() > boundary
            && !matches!(item.as_rule(), Rule::EOI | Rule::line_continuation)
    }) else {
        return Some(suffix.to_owned());
    };
    if item.as_rule() == Rule::comment {
        return None;
    }
    let end = item.as_span().end();
    let separator = full[end..]
        .chars()
        .next()
        .filter(|ch| SHELL_WHITESPACE.contains(ch))
        .map_or(0, char::len_utf8);
    Some(full[boundary..end + separator].to_owned())
}
