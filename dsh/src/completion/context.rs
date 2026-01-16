use super::command::CommandCompletionDatabase;
use super::parser::{CompletionContext, ParsedCommandLine};

/// Responsible for correcting parsed command line based on completion database
pub struct ContextCorrector<'a> {
    database: &'a CommandCompletionDatabase,
}

impl<'a> ContextCorrector<'a> {
    pub fn new(database: &'a CommandCompletionDatabase) -> Self {
        Self { database }
    }

    /// Correct the parsed command line if the parser incorrectly identified arguments as subcommands.
    pub fn correct_parsed_command_line(&self, parsed: &ParsedCommandLine) -> ParsedCommandLine {
        let Some(command_completion) = self.database.get_command(&parsed.command) else {
            return parsed.clone();
        };

        let mut new_parsed = parsed.clone();
        let mut valid_subcommands = Vec::new();
        let mut current_subcommands = &command_completion.subcommands;
        let mut split_index = parsed.subcommand_path.len();

        // 1. Identify valid subcommand chain
        for (idx, sub_name) in parsed.subcommand_path.iter().enumerate() {
            if let Some(sub) = current_subcommands.iter().find(|s| &s.name == sub_name) {
                valid_subcommands.push(sub_name.clone());
                current_subcommands = &sub.subcommands;
            } else {
                split_index = idx;
                break;
            }
        }

        // 2. Handle the split point
        // If we consumed everything, or stopped at the last token
        if split_index < parsed.subcommand_path.len() {
            // We have some invalid subcommands.
            let invalid_tokens = &parsed.subcommand_path[split_index..];
            let first_invalid = &invalid_tokens[0];
            let is_last_token = split_index + 1 == parsed.subcommand_path.len();
            let is_completion_target = first_invalid == &parsed.current_token;

            if is_last_token && is_completion_target {
                // The cursor is on this token.
                // Check if it is a partial subcommand match
                let is_partial_subcommand = current_subcommands
                    .iter()
                    .any(|s| s.name.starts_with(first_invalid));

                // Check if it looks like an option
                let is_option = first_invalid.starts_with('-');

                if is_partial_subcommand && !is_option {
                    // Treat as SubCommand completion
                    new_parsed.subcommand_path = valid_subcommands;
                    // We don't push the partial token to valid_subcommands for the path,
                    // but we do want the context to be SubCommand.
                    new_parsed.completion_context = CompletionContext::SubCommand;
                    return new_parsed;
                } else if is_option {
                    // Check original context to decide Short vs Long, or default to Long
                    if parsed.completion_context == CompletionContext::ShortOption {
                        new_parsed.completion_context = CompletionContext::ShortOption;
                    } else {
                        new_parsed.completion_context = CompletionContext::LongOption;
                    }
                    // The token is an option, so valid subcommands path stops before it.
                    new_parsed.subcommand_path = valid_subcommands;
                    return new_parsed;
                }
            }

            // If strictly invalid (not a partial match, or intermediate token), treat as arguments.
            let mut new_args = invalid_tokens.to_vec();
            new_args.extend(new_parsed.specified_arguments);
            new_parsed.specified_arguments = new_args;

            // Also update raw_args to include these tokens, as they are no longer subcommands
            // and should be considered part of the raw arguments for wrapped command parsing.
            let mut new_raw = invalid_tokens.to_vec();
            new_raw.extend(new_parsed.raw_args);
            new_parsed.raw_args = new_raw;

            new_parsed.subcommand_path = valid_subcommands;

            // Recalculate completion context
            // If user explicitly typed an option (starting with -), preserve Option context.
            if matches!(
                parsed.completion_context,
                CompletionContext::ShortOption | CompletionContext::LongOption
            ) {
                new_parsed.completion_context = parsed.completion_context.clone();
            } else {
                let arg_index = new_parsed.specified_arguments.len().saturating_sub(
                    if new_parsed
                        .specified_arguments
                        .contains(&new_parsed.current_token)
                    {
                        1
                    } else {
                        0
                    },
                );
                new_parsed.completion_context = CompletionContext::Argument {
                    arg_index,
                    arg_type: None,
                };
            }
            return new_parsed;
        }

        // Special Case: Context is SubCommand (from parser heuristic), but the matched command
        // has NO subcommands (e.g., `sudo`). In this case, we should treat it as Argument context.
        if parsed.completion_context == CompletionContext::SubCommand
            && current_subcommands.is_empty()
        {
            new_parsed.completion_context = CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            };
            return new_parsed;
        }

        // 3. Fallback: consumed all subcommands successfully
        // If the context was Argument but we are actually at a point where subcommands are possible?
        if matches!(
            new_parsed.completion_context,
            CompletionContext::Argument { .. }
        ) {
            // Check for combined short options in the previous token
            let check_index = if new_parsed.current_token.is_empty() {
                new_parsed.raw_args.len().checked_sub(1)
            } else {
                new_parsed.raw_args.len().checked_sub(2)
            };

            if let Some(idx) = check_index
                && let Some(prev_token) = new_parsed.raw_args.get(idx)
                && prev_token.starts_with('-')
                && !prev_token.starts_with("--")
                && prev_token.len() > 2
            {
                // Collect available options for current scope
                let mut available_options = command_completion.global_options.clone();
                let mut curr_subs = &command_completion.subcommands;
                for sub_name in &new_parsed.subcommand_path {
                    if let Some(sub) = curr_subs.iter().find(|s| &s.name == sub_name) {
                        available_options.extend(sub.options.clone());
                        curr_subs = &sub.subcommands;
                    } else {
                        break;
                    }
                }

                // Check the last character of the combined option
                if let Some(last_char) = prev_token.chars().last() {
                    let short_name = format!("-{}", last_char);
                    if let Some(opt) = available_options
                        .iter()
                        .find(|o| o.short.as_ref() == Some(&short_name))
                        && opt.argument.is_some()
                    {
                        // The last flag requires an argument, so the current token is its value
                        new_parsed.completion_context = CompletionContext::OptionValue {
                            option_name: short_name,
                            value_type: None, // Will be resolved by generator
                        };
                        return new_parsed;
                    }
                }
            }

            // Subcommand fallback check (existing logic)
            if !current_subcommands.is_empty()
                && current_subcommands
                    .iter()
                    .any(|s| s.name.starts_with(&new_parsed.current_token))
            {
                new_parsed.completion_context = CompletionContext::SubCommand;
            }
        }

        new_parsed
    }
}
