use super::command::{
    ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption, SubCommand,
};
use super::parser::{
    CommandLineParser, CompletionContext, ParsedCommandLine, split_inline_long_option,
};

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

        for token in &parsed.raw_args {
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
            } else {
                // Token doesn't match any subcommand, stop scanning
                break;
            }
        }

        if subcommands_found {
            new_parsed.subcommand_path = valid_subcommands;

            // Clean up consumed tokens from options/args lists is complex and maybe unnecessary
            // as generators usually rely on context and subcommand_path.
            // But we should update completion context if we consumed everything and are now looking at arguments.

            // Recalculate context based on what remains
            if parsed.raw_args.len() > tokens_consumed {
                // We have remaining args.
                // The next one is the current context?
                // If cursor is on one of the remaining, context is Argument?
                new_parsed.completion_context = CompletionContext::Argument {
                    arg_index: new_parsed.specified_arguments.len(), // approximate
                    arg_type: None,
                };

                // If the immediate next token is what we are completing
                if parsed.raw_args.len() == tokens_consumed + 1
                    && parsed.raw_args[tokens_consumed] == new_parsed.current_token
                {
                    // We are completing the first argument
                    new_parsed.completion_context = CompletionContext::Argument {
                        arg_index: 0,
                        arg_type: None,
                    };
                }
            } else {
                // Consumed all raw args as path? Then we are expecting new args/options
                // If we ended exactly on a subcommand (and it wasn't current_token check above?),
                // then we are completing arguments of that subcommand.
                new_parsed.completion_context = CompletionContext::Argument {
                    arg_index: 0,
                    arg_type: None,
                };
            }

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

    fn correct_option_value_context(
        &self,
        parsed: &ParsedCommandLine,
        command_completion: &CommandCompletion,
    ) -> Option<ParsedCommandLine> {
        let options = self.collect_available_options(command_completion, &parsed.subcommand_path);

        if let Some(raw_token) = Self::current_raw_token(parsed)
            && let Some((option_name, value)) = split_inline_long_option(raw_token)
        {
            let mut corrected = parsed.clone();
            corrected.current_token = value.to_string();
            corrected.current_arg = Some(corrected.current_token.clone());

            if let Some(option) = options
                .iter()
                .find(|option| option.matches_name(option_name))
            {
                if option.expects_value() {
                    corrected.completion_context = CompletionContext::OptionValue {
                        option_name: option_name.to_string(),
                        value_type: option.value_type().cloned(),
                    };
                    corrected.specified_options = normalize_specified_option(
                        &corrected.specified_options,
                        raw_token,
                        option_name,
                    );
                    corrected.options = corrected.specified_options.clone();
                    return Some(corrected);
                }

                corrected.current_token = raw_token.to_string();
                corrected.current_arg = Some(corrected.current_token.clone());
                corrected.completion_context = CompletionContext::LongOption;
                return Some(corrected);
            }
        }

        if let Some(raw_token) = Self::current_raw_token(parsed)
            && let Some((option_name, value_type, value)) =
                split_attached_short_option_value(raw_token, &options)
        {
            let mut corrected = parsed.clone();
            corrected.current_token = value.to_string();
            corrected.current_arg = Some(corrected.current_token.clone());
            corrected.completion_context = CompletionContext::OptionValue {
                option_name: option_name.clone(),
                value_type,
            };
            corrected.specified_options =
                normalize_specified_option(&corrected.specified_options, raw_token, &option_name);
            corrected.options = corrected.specified_options.clone();
            return Some(corrected);
        }

        if parsed.current_token.starts_with('-') {
            return None;
        }

        let previous = Self::previous_raw_token(parsed)?;
        let option = options
            .into_iter()
            .find(|option| option.matches_name(previous) && option.expects_value())?;

        let mut corrected = parsed.clone();
        corrected.completion_context = CompletionContext::OptionValue {
            option_name: previous.to_string(),
            value_type: option.value_type().cloned(),
        };
        Self::remove_current_value_from_arguments(&mut corrected);
        Some(corrected)
    }

    fn remove_known_option_values_from_arguments(
        &self,
        parsed: &mut ParsedCommandLine,
        command_completion: &CommandCompletion,
    ) {
        let options = self.collect_available_options(command_completion, &parsed.subcommand_path);
        let arguments = Self::specified_arguments_without_known_option_values(parsed, &options);

        if arguments == parsed.specified_arguments {
            return;
        }

        parsed.specified_arguments = arguments;
        parsed.args = parsed.specified_arguments.clone();
        Self::recalculate_argument_context(parsed);
    }

    fn specified_arguments_without_known_option_values(
        parsed: &ParsedCommandLine,
        options: &[&CommandOption],
    ) -> Vec<String> {
        let mut rebuilt = Vec::with_capacity(parsed.specified_arguments.len());
        let mut specified_index = 0;
        let mut raw_index = 0;
        let mut skip_next_redirect_target = false;
        let mut end_of_options = false;

        while raw_index < parsed.raw_args.len() {
            let token = parsed.raw_args[raw_index].as_str();

            if skip_next_redirect_target {
                skip_next_redirect_target = false;
                raw_index += 1;
                continue;
            }

            if !end_of_options && Self::is_redirect_operator(token) {
                skip_next_redirect_target = true;
                raw_index += 1;
                continue;
            }

            if !end_of_options && token == "--" {
                end_of_options = true;
                raw_index += 1;
                continue;
            }

            if !end_of_options && Self::is_inline_long_option_value(token, options) {
                raw_index += 1;
                continue;
            }

            if !end_of_options && split_attached_short_option_value(token, options).is_some() {
                raw_index += 1;
                continue;
            }

            if !end_of_options
                && is_separate_value_option(token, options)
                && let Some(value) = parsed.raw_args.get(raw_index + 1)
                && !Self::looks_like_known_option(value, options)
            {
                if parsed
                    .specified_arguments
                    .get(specified_index)
                    .is_some_and(|argument| argument == value)
                {
                    specified_index += 1;
                }
                raw_index += 2;
                continue;
            }

            if parsed
                .specified_arguments
                .get(specified_index)
                .is_some_and(|argument| argument == token)
            {
                rebuilt.push(token.to_string());
                specified_index += 1;
            }

            raw_index += 1;
        }

        rebuilt.extend(
            parsed
                .specified_arguments
                .iter()
                .skip(specified_index)
                .cloned(),
        );
        rebuilt
    }

    fn is_inline_long_option_value(token: &str, options: &[&CommandOption]) -> bool {
        if let Some((option_name, _)) = split_inline_long_option(token) {
            return options
                .iter()
                .any(|option| option.matches_name(option_name) && option.expects_value());
        }
        false
    }

    fn looks_like_known_option(token: &str, options: &[&CommandOption]) -> bool {
        if let Some((option_name, _)) = split_inline_long_option(token) {
            return options
                .iter()
                .any(|option| option.matches_name(option_name));
        }

        options.iter().any(|option| option.matches_name(token))
            || split_attached_short_option_value(token, options).is_some()
    }

    fn is_redirect_operator(token: &str) -> bool {
        if matches!(token, ">" | ">>" | "<" | "&>" | "&>>") {
            return true;
        }

        if let Some(prefix) = token.strip_suffix(">>") {
            return prefix.is_empty() || prefix.chars().all(|c| c.is_ascii_digit());
        }

        if let Some(prefix) = token.strip_suffix('>') {
            return prefix.is_empty() || prefix.chars().all(|c| c.is_ascii_digit());
        }

        false
    }

    fn remove_current_value_from_arguments(parsed: &mut ParsedCommandLine) {
        if parsed.current_token.is_empty() {
            return;
        }

        if let Some(pos) = parsed
            .specified_arguments
            .iter()
            .rposition(|argument| argument == &parsed.current_token)
        {
            parsed.specified_arguments.remove(pos);
            parsed.args = parsed.specified_arguments.clone();
        }
    }

    fn recalculate_argument_context(parsed: &mut ParsedCommandLine) {
        let CompletionContext::Argument { arg_type, .. } = &parsed.completion_context else {
            return;
        };

        let current_is_counted = parsed
            .specified_arguments
            .iter()
            .any(|argument| argument == &parsed.current_token);
        let arg_index = parsed
            .specified_arguments
            .len()
            .saturating_sub(usize::from(current_is_counted));
        parsed.completion_context = CompletionContext::Argument {
            arg_index,
            arg_type: arg_type.clone(),
        };
    }

    fn current_raw_token(parsed: &ParsedCommandLine) -> Option<&str> {
        let index = Self::current_raw_index(parsed)?;
        parsed.raw_args.get(index).map(String::as_str)
    }

    fn current_raw_index(parsed: &ParsedCommandLine) -> Option<usize> {
        if parsed.raw_args.is_empty() {
            return None;
        }

        if parsed.current_token.is_empty()
            && let Some(index) = parsed.raw_args.iter().rposition(|token| {
                split_inline_long_option(token).is_some_and(|(_, value)| value.is_empty())
            })
        {
            return Some(index);
        }

        if parsed.current_token.is_empty()
            && let Some(index) = parsed.raw_args.iter().rposition(|token| token.is_empty())
        {
            return Some(index);
        }

        if !parsed.current_token.is_empty()
            && let Some(index) = parsed.raw_args.iter().rposition(|token| {
                token == &parsed.current_token
                    || split_inline_long_option(token)
                        .is_some_and(|(_, value)| value == parsed.current_token)
            })
        {
            return Some(index);
        }

        Some(parsed.raw_args.len())
    }

    fn previous_raw_token(parsed: &ParsedCommandLine) -> Option<&str> {
        if parsed.raw_args.is_empty() {
            return None;
        }

        let current_index = Self::current_raw_index(parsed).unwrap_or(parsed.raw_args.len());

        parsed
            .raw_args
            .get(current_index.checked_sub(1)?)
            .map(String::as_str)
    }

    fn collect_available_options<'b>(
        &self,
        command_completion: &'b CommandCompletion,
        subcommand_path: &[String],
    ) -> Vec<&'b CommandOption> {
        let mut options = Vec::new();
        options.extend(&command_completion.global_options);

        let mut current_subcommands = &command_completion.subcommands;
        for subcommand_name in subcommand_path {
            let Some(subcommand) =
                Self::find_matching_subcommand(current_subcommands, subcommand_name)
            else {
                break;
            };
            options.extend(&subcommand.options);
            current_subcommands = &subcommand.subcommands;
        }

        options
    }

    fn find_matching_subcommand<'b>(
        subcommands: &'b [SubCommand],
        name: &str,
    ) -> Option<&'b SubCommand> {
        subcommands
            .iter()
            .find(|subcommand| Self::subcommand_matches(subcommand, name))
    }

    fn subcommand_matches(subcommand: &SubCommand, name: &str) -> bool {
        subcommand.name == name || subcommand.aliases.iter().any(|alias| alias == name)
    }

    fn subcommand_starts_with(subcommand: &SubCommand, prefix: &str) -> bool {
        subcommand.name.starts_with(prefix)
            || subcommand
                .aliases
                .iter()
                .any(|alias| alias.starts_with(prefix))
    }

    pub fn find_command_with_args_arg(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Option<(usize, String)> {
        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let args_def = &command_completion.arguments;
            for (i, arg_val) in parsed.specified_arguments.iter().enumerate() {
                if let Some(arg_def) = args_def.get(i)
                    && let Some(ArgumentType::CommandWithArgs) = arg_def.arg_type
                {
                    return Some((i, arg_val.clone()));
                }
            }
        }
        None
    }

    pub fn reparse_inner_command(
        &self,
        parsed: &ParsedCommandLine,
        cmd_index: usize,
        cmd_name: String,
    ) -> ParsedCommandLine {
        let mut input_parts = Vec::new();
        input_parts.push(cmd_name);

        let mut found_start = false;

        let target_arg = &parsed.specified_arguments[cmd_index];
        let mut tokens_to_skip = 0;

        for (i, token) in parsed.raw_args.iter().enumerate() {
            if token == target_arg {
                tokens_to_skip = i + 1;
                found_start = true;
                break;
            }
        }

        if found_start {
            for arg in parsed.raw_args.iter().skip(tokens_to_skip) {
                if arg.contains(' ') || arg.contains('\t') {
                    input_parts.push(format!("{:?}", arg));
                } else {
                    input_parts.push(arg.to_string());
                }
            }
        }

        // When cursor is in a trailing gap (empty current_token), append empty string
        // to signal the parser about the gap position
        if parsed.current_token.is_empty() {
            input_parts.push(String::new());
        }

        let input = input_parts.join(" ");
        CommandLineParser::new().parse(&input, input.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::command::{ArgumentType, CommandCompletion, CommandOption, SubCommand};

    #[test]
    fn test_correct_flag_like_subcommand() {
        let mut db = CommandCompletionDatabase::new();
        let completion = CommandCompletion {
            command: "pacman".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "-S".to_string(),
                description: None,
                aliases: vec![],
                options: vec![],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        };
        db.add_command(completion);

        let corrector = ContextCorrector::new(&db);

        // Case: "pacman -S" where parser initially thought -S was an option
        let parsed = ParsedCommandLine {
            command: "pacman".to_string(),
            subcommand_path: vec![],
            // Raw args might just be ["-S"] if "pacman" was consumed as command
            raw_args: vec!["-S".to_string()],
            args: vec![],
            options: vec![],
            current_token: "-S".to_string(),
            current_arg: None,
            completion_context: CompletionContext::ShortOption,
            specified_options: vec!["-S".to_string()],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        let corrected = corrector.correct_parsed_command_line(&parsed);

        // This assertion is expected to FAIL before the fix
        assert!(
            matches!(corrected.completion_context, CompletionContext::SubCommand),
            "Expected SubCommand context, got {:?}",
            corrected.completion_context
        );
    }

    #[test]
    fn test_correct_alias_subcommand() {
        let mut db = CommandCompletionDatabase::new();
        let completion = CommandCompletion {
            command: "pacman".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "-S".to_string(),
                description: None,
                aliases: vec!["--sync".to_string()],
                options: vec![],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        };
        db.add_command(completion);

        let corrector = ContextCorrector::new(&db);

        // Case: "pacman --sync"
        let parsed = ParsedCommandLine {
            command: "pacman".to_string(),
            subcommand_path: vec![],
            raw_args: vec!["--sync".to_string()],
            args: vec![],
            options: vec![],
            current_token: "--sync".to_string(), // Current token is the alias
            current_arg: None,
            completion_context: CompletionContext::LongOption, // Parser thinks it's a long option
            specified_options: vec!["--sync".to_string()],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        let corrected = corrector.correct_parsed_command_line(&parsed);

        // Should be corrected to SubCommand context because --sync is alias of -S
        assert!(
            matches!(corrected.completion_context, CompletionContext::SubCommand),
            "Expected SubCommand context for alias, got {:?}",
            corrected.completion_context
        );
    }

    #[test]
    fn alias_subcommand_is_canonicalized_before_argument_completion() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "pacman".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "-S".to_string(),
                description: None,
                aliases: vec!["--sync".to_string()],
                options: vec![],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("pacman --sync ", "pacman --sync ".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.subcommand_path, vec!["-S".to_string()]);
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::Argument { arg_index: 0, .. }
        ));
    }

    #[test]
    fn option_value_context_uses_completion_definition() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "cargo".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "test".to_string(),
                description: None,
                aliases: vec![],
                options: vec![CommandOption {
                    short: Some("-p".to_string()),
                    long: Some("--package".to_string()),
                    description: None,
                    takes_value: true,
                    value_type: Some(ArgumentType::Choice(vec!["doge-shell".to_string()])),
                    argument: None,
                }],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("cargo test -p do", "cargo test -p do".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert!(matches!(
            corrected.completion_context,
            CompletionContext::OptionValue {
                option_name,
                value_type: Some(ArgumentType::Choice(_))
            } if option_name == "-p"
        ));
        assert!(corrected.specified_arguments.is_empty());
        assert!(corrected.args.is_empty());
    }

    #[test]
    fn known_option_value_is_removed_before_positional_argument_index() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "pytest".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-k".to_string()),
                long: None,
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::String),
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![crate::completion::command::Argument {
                name: "path".to_string(),
                description: None,
                multiple: true,
                arg_type: None,
            }],
        });

        let parsed =
            CommandLineParser::new().parse("pytest -k expr tests", "pytest -k expr tests".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.specified_arguments, vec!["tests".to_string()]);
        assert_eq!(corrected.args, vec!["tests".to_string()]);
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::Argument { arg_index: 0, .. }
        ));
    }

    #[test]
    fn known_option_value_removal_preserves_same_text_positional_argument() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "cmd".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-k".to_string()),
                long: None,
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::String),
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![crate::completion::command::Argument {
                name: "path".to_string(),
                description: None,
                multiple: true,
                arg_type: None,
            }],
        });

        let parsed =
            CommandLineParser::new().parse("cmd foo -k foo bar", "cmd foo -k foo bar".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(
            corrected.specified_arguments,
            vec!["foo".to_string(), "bar".to_string()]
        );
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::Argument { arg_index: 1, .. }
        ));
    }

    #[test]
    fn known_option_value_is_removed_before_empty_positional_argument() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "pytest".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-k".to_string()),
                long: None,
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::String),
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![crate::completion::command::Argument {
                name: "path".to_string(),
                description: None,
                multiple: true,
                arg_type: None,
            }],
        });

        let parsed = CommandLineParser::new().parse("pytest -k expr ", "pytest -k expr ".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.specified_arguments, vec!["".to_string()]);
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::Argument { arg_index: 0, .. }
        ));
    }

    #[test]
    fn inline_long_option_value_uses_completion_definition() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "kubectl".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: None,
                long: Some("--context".to_string()),
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::Choice(vec!["dev-cluster".to_string()])),
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![],
        });

        let parsed =
            CommandLineParser::new().parse("kubectl --context=de", "kubectl --context=de".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.current_token, "de");
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::OptionValue {
                option_name,
                value_type: Some(ArgumentType::Choice(_))
            } if option_name == "--context"
        ));
        assert_eq!(corrected.raw_args, vec!["--context=de".to_string()]);
        assert_eq!(corrected.specified_options, vec!["--context".to_string()]);
    }

    #[test]
    fn short_attached_option_value_uses_completion_definition() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "kubectl".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-n".to_string()),
                long: Some("--namespace".to_string()),
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::Choice(vec!["dev-namespace".to_string()])),
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("kubectl -nde", "kubectl -nde".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.current_token, "de");
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::OptionValue {
                option_name,
                value_type: Some(ArgumentType::Choice(_))
            } if option_name == "-n"
        ));
        assert_eq!(corrected.raw_args, vec!["-nde".to_string()]);
        assert_eq!(corrected.specified_options, vec!["-n".to_string()]);
        assert_eq!(corrected.options, vec!["-n".to_string()]);
    }

    #[test]
    fn short_attached_option_without_value_definition_does_not_use_value_provider() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "cmd".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-v".to_string()),
                long: Some("--verbose".to_string()),
                description: None,
                takes_value: false,
                value_type: None,
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("cmd -vfoo", "cmd -vfoo".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.current_token, "-vfoo");
        assert_eq!(corrected.completion_context, CompletionContext::LongOption);
        assert_eq!(corrected.specified_options, vec!["-vfoo".to_string()]);
    }

    #[test]
    fn short_equals_option_value_stays_out_of_scope() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "cmd".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-x".to_string()),
                long: Some("--example".to_string()),
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::Choice(vec!["value".to_string()])),
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("cmd -x=y", "cmd -x=y".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.current_token, "-x=y");
        assert_eq!(corrected.completion_context, CompletionContext::LongOption);
        assert_eq!(corrected.specified_options, vec!["-x=y".to_string()]);
    }

    #[test]
    fn separate_option_empty_value_uses_completion_definition() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "cargo".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "test".to_string(),
                description: None,
                aliases: vec![],
                options: vec![CommandOption {
                    short: Some("-p".to_string()),
                    long: Some("--package".to_string()),
                    description: None,
                    takes_value: true,
                    value_type: Some(ArgumentType::Choice(vec!["doge-shell".to_string()])),
                    argument: None,
                }],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("cargo test -p ", "cargo test -p ".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.current_token, "");
        assert!(matches!(
            corrected.completion_context,
            CompletionContext::OptionValue {
                option_name,
                value_type: Some(ArgumentType::Choice(_))
            } if option_name == "-p"
        ));
    }

    #[test]
    fn inline_long_option_without_value_definition_does_not_use_value_provider() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(CommandCompletion {
            command: "cmd".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: None,
                long: Some("--verbose".to_string()),
                description: None,
                takes_value: false,
                value_type: None,
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![],
        });

        let parsed = CommandLineParser::new().parse("cmd --verbose=x", "cmd --verbose=x".len());
        let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

        assert_eq!(corrected.current_token, "--verbose=x");
        assert_eq!(corrected.completion_context, CompletionContext::LongOption);
    }
}
