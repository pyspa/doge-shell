//! Candidate scoring and argument-type resolution: history-frequency boosts,
//! and walking a command's subcommand/argument/option tree to find the
//! `ArgumentType` that applies at the cursor.
use super::*;
use crate::completion::parser;
use std::collections::{HashMap, HashSet};

pub(super) fn history_boost_scores(
    candidates: &[EnhancedCandidate],
    history: &crate::history::History,
    command_context: Option<&str>,
) -> Vec<u32> {
    let mut scores = vec![0; candidates.len()];
    let mut candidate_indexes_by_token: HashMap<&str, Vec<usize>> = HashMap::new();

    for (index, candidate) in candidates.iter().enumerate() {
        if matches!(
            candidate.candidate_type,
            CandidateType::File | CandidateType::Directory
        ) {
            continue;
        }

        candidate_indexes_by_token
            .entry(candidate.text.as_str())
            .or_default()
            .push(index);
    }

    if candidate_indexes_by_token.is_empty() {
        return scores;
    }

    let command_prefix = command_context.map(|command| format!("{command} "));
    let mut capped = vec![false; candidates.len()];
    let mut capped_count = 0;
    let target_count = candidate_indexes_by_token
        .values()
        .map(Vec::len)
        .sum::<usize>();

    for item in history.iter().rev().take(HISTORY_BOOST_SCAN_LIMIT) {
        let context_bonus = if command_context.is_some_and(|command| item.entry == command)
            || command_prefix
                .as_ref()
                .is_some_and(|prefix| item.entry.starts_with(prefix))
        {
            500
        } else {
            0
        };

        let mut tokens_seen = HashSet::new();
        for token in item.entry.split_whitespace() {
            if !tokens_seen.insert(token) {
                continue;
            }

            let Some(indexes) = candidate_indexes_by_token.get(token) else {
                continue;
            };

            for &index in indexes {
                if capped[index] {
                    continue;
                }

                scores[index] = scores[index].saturating_add(10 + context_bonus);
                if scores[index] > HISTORY_BOOST_SCORE_CAP {
                    capped[index] = true;
                    capped_count += 1;
                }
            }
        }

        if capped_count == target_count {
            break;
        }
    }

    scores
}

pub(super) fn argument_type_for_completion_context(
    database: &CommandCompletionDatabase,
    parsed: &ParsedCommandLine,
) -> Option<ArgumentType> {
    match &parsed.completion_context {
        parser::CompletionContext::Argument {
            arg_type: Some(arg_type),
            ..
        } => Some(arg_type.clone()),
        parser::CompletionContext::Argument { arg_index, .. } => {
            let command_completion = database.get_command(&parsed.command)?;
            let arguments =
                arguments_for_subcommand_path(command_completion, &parsed.subcommand_path);
            resolve_argument_definition(arguments, *arg_index)
                .and_then(|argument| argument.arg_type.clone())
        }
        parser::CompletionContext::OptionValue {
            option_name,
            value_type: Some(value_type),
        } => {
            let command_completion = database.get_command(&parsed.command)?;
            let option_value_type = option_for_subcommand_path(
                command_completion,
                &parsed.subcommand_path,
                option_name,
            )
            .and_then(CommandOption::value_type)
            .cloned();
            option_value_type.or_else(|| Some(value_type.clone()))
        }
        parser::CompletionContext::OptionValue {
            option_name,
            value_type: None,
        } => {
            let command_completion = database.get_command(&parsed.command)?;
            option_for_subcommand_path(command_completion, &parsed.subcommand_path, option_name)
                .and_then(CommandOption::value_type)
                .cloned()
        }
        _ => None,
    }
}

pub(super) fn arguments_for_subcommand_path<'a>(
    command_completion: &'a crate::completion::command::CommandCompletion,
    path: &[String],
) -> &'a [crate::completion::command::Argument] {
    let mut arguments = &command_completion.arguments;
    let mut subcommands = &command_completion.subcommands;

    for name in path {
        let Some(subcommand) = find_matching_subcommand(subcommands, name) else {
            break;
        };
        arguments = &subcommand.arguments;
        subcommands = &subcommand.subcommands;
    }

    arguments
}

pub(super) fn option_for_subcommand_path<'a>(
    command_completion: &'a crate::completion::command::CommandCompletion,
    path: &[String],
    option_name: &str,
) -> Option<&'a CommandOption> {
    let mut options = command_completion.global_options.iter().collect::<Vec<_>>();
    let mut subcommands = &command_completion.subcommands;

    for name in path {
        let Some(subcommand) = find_matching_subcommand(subcommands, name) else {
            break;
        };
        options.extend(subcommand.options.iter());
        subcommands = &subcommand.subcommands;
    }

    options
        .into_iter()
        .find(|option| option.matches_name(option_name))
}

pub(super) fn resolve_argument_definition(
    arguments: &[crate::completion::command::Argument],
    arg_index: usize,
) -> Option<&crate::completion::command::Argument> {
    arguments.get(arg_index).or_else(|| {
        arguments
            .last()
            .filter(|argument| argument.multiple && !arguments.is_empty())
    })
}

pub(super) fn find_matching_subcommand<'a>(
    subcommands: &'a [SubCommand],
    name: &str,
) -> Option<&'a SubCommand> {
    subcommands.iter().find(|subcommand| {
        subcommand.name == name || subcommand.aliases.iter().any(|alias| alias == name)
    })
}

pub(super) fn is_ghost_safe_argument_type(arg_type: &ArgumentType) -> bool {
    matches!(
        arg_type,
        ArgumentType::Choice(_)
            | ArgumentType::Environment
            | ArgumentType::Process
            | ArgumentType::User
            | ArgumentType::Group
            | ArgumentType::Signal
            | ArgumentType::Interface
            | ArgumentType::Dynamic { .. }
    )
}
