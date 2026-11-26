//! Cursor-aware interpretation of completion definitions over lexical source.
//! Resolve subcommand parents, option values, redirect targets and positional
//! indexes without evaluating shell syntax. Source identity survives repeated
//! correction and wrapper dispatch; raw provider metadata remains unchanged.
use super::command::{
    ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption, SubCommand,
};
use super::parser::{
    CommandLineParser, CompletionContext, ParsedCommandLine, split_inline_long_option,
};

mod option_values;
pub(super) use option_values::OptionWord;
mod pacman;
pub(crate) use pacman::pacman_short_path;
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
        if parsed
            .source
            .as_ref()
            .is_some_and(|source| source.suppressed)
            || parsed.completion_context == CompletionContext::Command
        {
            return parsed.clone();
        }
        if let Some(source) = &parsed.source
            && let Some(previous) = source
                .cursor_word
                .checked_sub(1)
                .and_then(|index| source.words.get(index))
            && Self::is_redirect_operator(&previous.raw)
        {
            let mut target = parsed.clone();
            target.completion_context = CompletionContext::Argument {
                arg_index: 0,
                arg_type: Some(ArgumentType::File { extensions: None }),
            };
            return target;
        }
        let mut corrected = self.correct_inner(parsed);
        // A lexical guess such as "sudo -n VALUE" can point at the wrapper's
        // command operand. Use its declared argv position once flags and their
        // values have been accounted for.
        if matches!(
            corrected.completion_context,
            CompletionContext::OptionValue {
                value_type: None,
                ..
            }
        ) && Self::current_raw_token(&corrected) == Some(corrected.current_token.as_str())
            && let Some((index, _)) = self.find_command_with_args_arg(&corrected)
            && let Some(raw_index) = self.argument_raw_index(&corrected, index)
            && Self::current_raw_index(&corrected) == Some(raw_index)
        {
            corrected.completion_context = CompletionContext::Argument {
                arg_index: index,
                arg_type: None,
            };
        }
        if corrected.source.is_some()
            && matches!(
                corrected.completion_context,
                CompletionContext::Argument { .. }
            )
            && let Some(definition) = self.database.get_command(&corrected.command)
        {
            let options = self.collect_available_options(definition, &corrected.subcommand_path);
            let start = corrected
                .source
                .as_ref()
                .map_or(0, |source| source.argument_start);
            let (_, index) =
                Self::positional_arguments_after_raw_index(&corrected, start, &options);
            let pacman_target = if corrected.command == "pacman" {
                Self::pacman_target_type(&corrected, &options, definition)
            } else {
                None
            };
            if let CompletionContext::Argument {
                arg_index,
                arg_type,
            } = &mut corrected.completion_context
            {
                *arg_index = index;
                if pacman_target.is_some() {
                    *arg_type = pacman_target;
                }
            }
        }
        corrected
    }

    fn correct_inner(&self, parsed: &ParsedCommandLine) -> ParsedCommandLine {
        let Some(command_completion) = self.database.get_command(&parsed.command) else {
            return parsed.clone();
        };

        let mut new_parsed = parsed.clone();
        let mut valid_subcommands = Vec::new();
        let mut current_subcommands = &command_completion.subcommands;
        let mut split_index = parsed.subcommand_path.len();

        // 1. Identify valid subcommand chain
        for (idx, sub_name) in parsed.subcommand_path.iter().enumerate() {
            if parsed
                .source
                .as_ref()
                .is_some_and(|source| source.cursor_word == source.command_word + idx + 1)
            {
                split_index = idx;
                break;
            }
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
            let invalid_tokens: Vec<String> = parsed.subcommand_path[split_index..]
                .iter()
                .enumerate()
                .map(|(offset, word)| {
                    let Some(source) = &parsed.source else {
                        return word.clone();
                    };
                    let index = source.command_word + split_index + offset + 1;
                    if index == source.cursor_word {
                        parsed.current_token.clone()
                    } else {
                        source
                            .words
                            .get(index)
                            .map_or_else(|| word.clone(), |span| span.raw.clone())
                    }
                })
                .collect();
            let first_invalid = &invalid_tokens[0];
            let is_last_token = split_index + 1 == parsed.subcommand_path.len();
            let is_completion_target = parsed.source.as_ref().map_or(
                is_last_token && first_invalid == &parsed.current_token,
                |source| source.cursor_word == source.command_word + split_index + 1,
            );

            if is_completion_target {
                // The cursor is on this token.
                // Check if it is a partial subcommand match
                let is_partial_subcommand = current_subcommands
                    .iter()
                    .any(|s| Self::subcommand_starts_with(s, first_invalid));

                // Check if it looks like an option
                let is_option = first_invalid.starts_with('-');

                if is_partial_subcommand && !is_option {
                    // Treat as SubCommand completion
                    // Keep the cursor word in raw_args so a second correction
                    // retains its source identity and cannot consume suffix words.
                    let mut raw = invalid_tokens.to_vec();
                    raw.extend(new_parsed.raw_args);
                    new_parsed.raw_args = raw;
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
                let options =
                    self.collect_available_options(command_completion, &new_parsed.subcommand_path);
                let (_, arg_index) =
                    Self::positional_arguments_after_raw_index(&new_parsed, 0, &options);
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

        // Quotes/escapes are source style, not part of a subcommand prefix.
        // A nonmatching word belongs to arguments even when the parser guessed
        // SubCommand because its subcommand_path was empty.
        if new_parsed.completion_context == CompletionContext::SubCommand
            && !current_subcommands
                .iter()
                .any(|s| Self::subcommand_starts_with(s, &new_parsed.completion_query()))
        {
            new_parsed.completion_context = CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            };
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
        ) && current_subcommands
            .iter()
            .any(|s| Self::subcommand_starts_with(s, &new_parsed.completion_query()))
        {
            new_parsed.completion_context = CompletionContext::SubCommand;
        }

        // 4. NEW: Scan raw_args for additional subcommands
        // The parser might have stopped subcommand parsing early (e.g. at a flag like -S).
        // If we still have valid subcommands in the definition, checkout raw_args.
        let mut tokens_consumed = 0;
        let mut subcommands_found = false;

        while tokens_consumed < parsed.raw_args.len() {
            let raw_token = &parsed.raw_args[tokens_consumed];
            let logical_token = super::shell_path::decode_literal_word(raw_token);
            let token = logical_token.as_str();
            // Shell redirections do not interrupt Git/global-option argv
            // scanning. A cursor on their target was handled above.
            if Self::is_redirect_operator(raw_token) {
                tokens_consumed += 2;
                continue;
            }
            // If this token is the current one being typed, don't consume it as a parent
            // But if it IS a valid subcommand, we should set context to SubCommand
            let is_current =
                Self::current_raw_index(&new_parsed).is_some_and(|index| index == tokens_consumed);

            // A completed pacman operation may carry short modifiers, e.g. -Syu.
            // Keep the original argv for providers and for replacement spans.
            let sub = Self::find_matching_subcommand(current_subcommands, token).or_else(|| {
                if parsed.command == "pacman" && !is_current {
                    Self::pacman_bundled_operation(current_subcommands, token)
                } else {
                    None
                }
            });
            if let Some(sub) = sub {
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

                if let Some(word) = OptionWord::parse(token, &available_options) {
                    tokens_consumed += 1;
                    if word.takes_separate_value() && tokens_consumed < parsed.raw_args.len() {
                        tokens_consumed =
                            Self::next_argv_index(&parsed.raw_args, tokens_consumed) + 1;
                    }
                    continue;
                }

                // Pacman also accepts operation-specific flags before the operation.
                if parsed.command == "pacman" && valid_subcommands.is_empty() && token != "--" {
                    let name = token.split('=').next().unwrap_or(token);
                    let takes_value = !token.contains('=')
                        && current_subcommands
                            .iter()
                            .flat_map(|sub| &sub.options)
                            .any(|opt| opt.matches_name(name) && opt.takes_separate_value());
                    tokens_consumed += 1;
                    if takes_value {
                        tokens_consumed =
                            Self::next_argv_index(&parsed.raw_args, tokens_consumed) + 1;
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
            if let Some(source) = &mut new_parsed.source {
                source.argument_start = tokens_consumed;
            }
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
