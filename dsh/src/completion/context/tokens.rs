//! The small questions the corrector asks about one token -- is it an inline
//! long-option value, a known option, a redirect operator -- plus the accessors
//! for the raw token under and before the cursor, the option/subcommand lookup
//! they share, and the inner-command reparse for wrappers like `sudo`.
use super::*;

impl ContextCorrector<'_> {
    pub(super) fn is_inline_long_option_value(token: &str, options: &[&CommandOption]) -> bool {
        if let Some((option_name, _)) = split_inline_long_option(token) {
            return options
                .iter()
                .any(|option| option.matches_name(option_name) && option.expects_value());
        }
        false
    }

    pub(super) fn looks_like_known_option(token: &str, options: &[&CommandOption]) -> bool {
        if let Some((option_name, _)) = split_inline_long_option(token) {
            return options
                .iter()
                .any(|option| option.matches_name(option_name));
        }

        options.iter().any(|option| option.matches_name(token))
            || split_attached_short_option_value(token, options).is_some()
    }

    pub(super) fn is_redirect_operator(token: &str) -> bool {
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

    pub(super) fn remove_current_value_from_arguments(parsed: &mut ParsedCommandLine) {
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

    pub(super) fn recalculate_argument_context(parsed: &mut ParsedCommandLine) {
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

    pub(super) fn current_raw_token(parsed: &ParsedCommandLine) -> Option<&str> {
        let index = Self::current_raw_index(parsed)?;
        parsed.raw_args.get(index).map(String::as_str)
    }

    pub(super) fn current_raw_index(parsed: &ParsedCommandLine) -> Option<usize> {
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

    pub(super) fn previous_raw_token(parsed: &ParsedCommandLine) -> Option<&str> {
        if parsed.raw_args.is_empty() {
            return None;
        }

        let current_index = Self::current_raw_index(parsed).unwrap_or(parsed.raw_args.len());

        parsed
            .raw_args
            .get(current_index.checked_sub(1)?)
            .map(String::as_str)
    }

    pub(super) fn collect_available_options<'b>(
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

    pub(super) fn find_matching_subcommand<'b>(
        subcommands: &'b [SubCommand],
        name: &str,
    ) -> Option<&'b SubCommand> {
        subcommands
            .iter()
            .find(|subcommand| Self::subcommand_matches(subcommand, name))
    }

    pub(super) fn subcommand_matches(subcommand: &SubCommand, name: &str) -> bool {
        subcommand.name == name || subcommand.aliases.iter().any(|alias| alias == name)
    }

    pub(super) fn subcommand_starts_with(subcommand: &SubCommand, prefix: &str) -> bool {
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
