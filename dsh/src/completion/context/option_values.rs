//! Deciding whether the token under the cursor is the *value* of an option
//! rather than a positional argument, and rebuilding the parsed line's argument
//! list once the option's own value has been accounted for.
use super::*;

impl ContextCorrector<'_> {
    pub(super) fn positional_arguments_after_raw_index(
        parsed: &ParsedCommandLine,
        start_index: usize,
        options: &[&CommandOption],
    ) -> (Vec<String>, usize) {
        let current_index = Self::current_raw_index(parsed).unwrap_or(parsed.raw_args.len());
        let indices = Self::positional_raw_indices(parsed, start_index, options);
        let arg_index = indices
            .iter()
            .filter(|&&index| index < current_index)
            .count();
        (
            indices
                .into_iter()
                .map(|index| parsed.raw_args[index].clone())
                .collect(),
            arg_index,
        )
    }

    pub(super) fn positional_raw_indices(
        parsed: &ParsedCommandLine,
        start_index: usize,
        options: &[&CommandOption],
    ) -> Vec<usize> {
        let mut indices = Vec::new();
        let mut index = start_index;
        let mut end_options = false;
        while let Some(token) = parsed.raw_args.get(index) {
            // Redirections belong to the shell, including after argv's `--`.
            if Self::is_redirect_operator(token) {
                index += 2;
                continue;
            }
            if !end_options && token == "--" {
                end_options = true;
                index += 1;
                continue;
            }
            if !end_options
                && (Self::is_inline_long_option_value(token, options)
                    || split_attached_short_option_value(token, options).is_some())
            {
                index += 1;
                continue;
            }
            if !end_options && is_separate_value_option(token, options) {
                index = Self::next_argv_index(&parsed.raw_args, index + 1) + 1;
                continue;
            }
            if !end_options && Self::looks_like_known_option(token, options) {
                index += 1;
                continue;
            }
            // env assignments precede the wrapped command. Inspect spelling
            // only: never evaluate a value or apply its environment/cwd changes.
            if parsed.command == "env"
                && indices.is_empty()
                && token.split_once('=').is_some_and(|(name, _)| {
                    !name.is_empty()
                        && name.chars().enumerate().all(|(i, c)| {
                            c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
                        })
                })
            {
                index += 1;
                continue;
            }
            indices.push(index);
            index += 1;
        }
        indices
    }

    pub(super) fn correct_option_value_context(
        &self,
        parsed: &ParsedCommandLine,
        command_completion: &CommandCompletion,
    ) -> Option<ParsedCommandLine> {
        let options = self.collect_available_options(command_completion, &parsed.subcommand_path);
        let current = Self::current_raw_index(parsed).unwrap_or(parsed.raw_args.len());
        if parsed
            .raw_args
            .iter()
            .take(current)
            .any(|token| token == "--")
        {
            return None;
        }

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

        let previous = Self::previous_argv_token(parsed)?;
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

    pub(super) fn remove_known_option_values_from_arguments(
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
        if let Some(source) = &parsed.source {
            return Self::positional_raw_indices(parsed, source.argument_start, options)
                .into_iter()
                .map(|index| parsed.raw_args[index].clone())
                .collect();
        }
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
}
