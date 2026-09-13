use super::command::{
    ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption, SubCommand,
};
use super::parser::{
    CommandLineParser, CompletionContext, ParsedCommandLine, split_inline_long_option,
};

mod option_values;
mod tokens;
fn normalize_specified_option(
    options: &[String],
    raw_token: &str,
    option_name: &str,
) -> Vec<String> {
    let mut normalized = options.to_vec();
    for option in &mut normalized {
        if option == raw_token {
            *option = option_name.to_string();
        }
    }
    if !normalized.iter().any(|option| option == option_name) {
        normalized.push(option_name.to_string());
    }
    normalized
}

fn split_attached_short_option_value<'a>(
    raw_token: &'a str,
    options: &[&CommandOption],
) -> Option<(String, Option<ArgumentType>, &'a str)> {
    if !raw_token.starts_with('-') || raw_token.starts_with("--") {
        return None;
    }

    options.iter().find_map(|option| {
        let short = option.short.as_deref()?;
        if short.len() != 2 || !option.expects_value() || !raw_token.starts_with(short) {
            return None;
        }

        let value = &raw_token[short.len()..];
        if value.is_empty() || value.starts_with('=') {
            return None;
        }

        Some((short.to_string(), option.value_type().cloned(), value))
    })
}

fn is_separate_value_option(raw_token: &str, options: &[&CommandOption]) -> bool {
    options
        .iter()
        .any(|option| option.matches_name(raw_token) && option.expects_value())
}

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
            if let Some(sub) = Self::find_matching_subcommand(current_subcommands, sub_name) {
                valid_subcommands.push(sub.name.clone());
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
                    .any(|s| Self::subcommand_starts_with(s, first_invalid));

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
            self.remove_known_option_values_from_arguments(&mut new_parsed, command_completion);
            if let Some(corrected) =
                self.correct_option_value_context(&new_parsed, command_completion)
            {
                return corrected;
            }
            return new_parsed;
        }

        self.remove_known_option_values_from_arguments(&mut new_parsed, command_completion);

        if let Some(corrected) = self.correct_option_value_context(&new_parsed, command_completion)
        {
            return corrected;
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
                    if let Some(sub) = Self::find_matching_subcommand(curr_subs, sub_name) {
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
                        && opt.expects_value()
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
                    .any(|s| Self::subcommand_starts_with(s, &new_parsed.current_token))
            {
                new_parsed.completion_context = CompletionContext::SubCommand;
            }
        }

        // 4. NEW: Scan raw_args for additional subcommands
        // The parser might have stopped subcommand parsing early (e.g. at a flag like -S).
        // If we still have valid subcommands in the definition, checkout raw_args.
        let mut tokens_consumed = 0;
        let mut subcommands_found = false;

        while tokens_consumed < parsed.raw_args.len() {
            let token = &parsed.raw_args[tokens_consumed];
            // If this token is the current one being typed, don't consume it as a parent
            // But if it IS a valid subcommand, we should set context to SubCommand
            let is_current = token == &new_parsed.current_token;

            if let Some(sub) = Self::find_matching_subcommand(current_subcommands, token) {
                if is_current {
                    // The current token IS a valid subcommand (e.g. pacman -S|)
                    new_parsed.completion_context = CompletionContext::SubCommand;
                    new_parsed.subcommand_path = valid_subcommands.clone();
                    return new_parsed;
                } else {
                    // It is a completed parent subcommand (e.g. pacman -S |package)
                    valid_subcommands.push(sub.name.clone());
                    current_subcommands = &sub.subcommands;
                    tokens_consumed += 1;
                    subcommands_found = true;
                }
            } else if is_current
                && current_subcommands
                    .iter()
                    .any(|s| Self::subcommand_starts_with(s, token))
            {
                new_parsed.completion_context = CompletionContext::SubCommand;
                new_parsed.subcommand_path = valid_subcommands.clone();
                return new_parsed;
            } else if !is_current {
                // Global options are commonly accepted before a subcommand (for example,
                // `snapper --config root delete`). Skip known options and their values so
                // subcommand recovery can continue after the parser stopped at the option.
                let available_options =
                    self.collect_available_options(command_completion, &valid_subcommands);

                if Self::is_inline_long_option_value(token, &available_options)
                    || split_attached_short_option_value(token, &available_options).is_some()
                {
                    tokens_consumed += 1;
                    continue;
                }

                if let Some(option) = available_options
                    .iter()
                    .find(|option| option.matches_name(token))
                {
                    tokens_consumed += 1;
                    if option.expects_value() && tokens_consumed < parsed.raw_args.len() {
                        tokens_consumed += 1;
                    }
                    continue;
                }

                break;
            } else {
                // Token doesn't match any subcommand, stop scanning
                break;
            }
        }

        if subcommands_found {
            new_parsed.subcommand_path = valid_subcommands;
            let available_options =
                self.collect_available_options(command_completion, &new_parsed.subcommand_path);
            let (arguments, arg_index) = Self::positional_arguments_after_raw_index(
                &new_parsed,
                tokens_consumed,
                &available_options,
            );
            new_parsed.specified_arguments = arguments;
            new_parsed.args = new_parsed.specified_arguments.clone();
            new_parsed.completion_context = CompletionContext::Argument {
                arg_index,
                arg_type: None,
            };

            // Override if current token looks like an option and we didn't match it as subcommand?
            if new_parsed.current_token.starts_with('-')
                && !current_subcommands
                    .iter()
                    .any(|s| Self::subcommand_matches(s, &new_parsed.current_token))
            {
                // e.g. pacman -S -y
                // -y is not subcommand.
                // So it is Option.
                // But wait, above logic sets context to Argument.
                // We need to return to Short/LongOption if it looks like one.
                if matches!(
                    parsed.completion_context,
                    CompletionContext::ShortOption | CompletionContext::LongOption
                ) {
                    new_parsed.completion_context = parsed.completion_context.clone();
                }
            }
        }

        self.remove_known_option_values_from_arguments(&mut new_parsed, command_completion);

        if let Some(corrected) = self.correct_option_value_context(&new_parsed, command_completion)
        {
            return corrected;
        }

        new_parsed
    }
}

#[cfg(test)]
mod tests;
