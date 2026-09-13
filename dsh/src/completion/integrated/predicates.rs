//! Small boolean predicates gating dynamic-completion behavior: prefix
//! matching, whether a parsed command line is a known dynamic provider,
//! whether its results are cacheable, and `git`-specific exclusivity/path
//! rules.
use super::*;
use crate::completion::fuzzy_match_score;
use crate::completion::parser;

pub(crate) fn matches_prefix(current_token: &str, value: &str) -> bool {
    current_token.is_empty()
        || value.starts_with(current_token)
        || fuzzy_match_score(value, current_token).is_some()
}

/// Whether the cursor sits past the wrapped command's own name token.
///
/// `sudo pac<TAB>` is completing the command name itself and must keep the
/// wrapper's own completion; `sudo pacman -R <TAB>` is completing *for* the
/// wrapped command and should be unwrapped.
pub(super) fn cursor_follows_wrapped_command(
    parsed: &ParsedCommandLine,
    command_name: &str,
) -> bool {
    let Some(command_index) = parsed
        .raw_args
        .iter()
        .position(|token| token == command_name)
    else {
        return false;
    };

    let cursor_index = if parsed.current_token.is_empty() {
        parsed.raw_args.len()
    } else {
        parsed
            .raw_args
            .iter()
            .rposition(|token| token == &parsed.current_token)
            .unwrap_or(parsed.raw_args.len())
    };

    cursor_index > command_index
}

pub(super) fn is_dynamic_completion_command(command: &str) -> bool {
    DYNAMIC_PROVIDER_SPECS
        .iter()
        .any(|provider| provider.command == command)
}

pub(super) fn completion_cache_allowed(parsed_command_line: &ParsedCommandLine) -> bool {
    // A wrapper (`sudo pacman -R`) parses as the wrapper's own command line, so
    // checking only `command` would let dynamic results be cached under the
    // wrapper. Scanning the arguments is cheap and errs toward not caching.
    let dynamic_command = is_dynamic_completion_command(&parsed_command_line.command)
        || parsed_command_line
            .specified_arguments
            .iter()
            .any(|argument| is_dynamic_completion_command(argument));

    if !dynamic_command {
        return true;
    }

    matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::Command
            | parser::CompletionContext::SubCommand
            | parser::CompletionContext::ShortOption
            | parser::CompletionContext::LongOption
    )
}

pub(super) fn dynamic_candidates_are_exclusive(parsed_command_line: &ParsedCommandLine) -> bool {
    if parsed_command_line.command != "git" {
        return false;
    }

    if !matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::Argument { .. } | parser::CompletionContext::SubCommand
    ) {
        return false;
    }

    let Some(primary_subcommand) = parsed_command_line.subcommand_path.first() else {
        return false;
    };

    matches!(
        primary_subcommand.as_str(),
        "checkout" | "switch" | "merge" | "rebase" | "branch"
    )
}

/// Whether the current `git` subcommand accepts working-tree paths as an
/// argument (in addition to any refs). `git checkout` and `git restore` are
/// dual-purpose (switch branch OR restore a file), so file/directory candidates
/// must be offered alongside branch candidates.
pub(super) fn git_subcommand_accepts_paths(parsed_command_line: &ParsedCommandLine) -> bool {
    if parsed_command_line.command != "git" {
        return false;
    }

    if !matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::Argument { .. }
    ) {
        return false;
    }

    matches!(
        parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str),
        Some("checkout") | Some("restore")
    )
}
