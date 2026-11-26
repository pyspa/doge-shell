//! Deciding whether the token under the cursor is the *value* of an option
//! rather than a positional argument, and rebuilding the parsed line's argument
//! list once the option's own value has been accounted for.
use super::*;

/// The option that determines how an argv word ends. For a short-option
/// cluster this is the first value-taking flag, or the final boolean flag.
pub(crate) struct OptionWord<'opt, 'raw> {
    name: &'opt str,
    option: &'opt CommandOption,
    attached: Option<&'raw str>,
}

impl<'opt, 'raw> OptionWord<'opt, 'raw> {
    pub(super) fn parse(raw: &'raw str, options: &[&'opt CommandOption]) -> Option<Self> {
        Self::visit(raw, options, |_| {})
    }

    /// Visit each declared spelling up to the first flag that owns a value.
    pub(crate) fn visit(
        raw: &'raw str,
        options: &[&'opt CommandOption],
        mut record: impl FnMut(&'opt str),
    ) -> Option<Self> {
        let (name, attached) =
            split_inline_long_option(raw).map_or((raw, None), |(name, value)| (name, Some(value)));
        let name = super::super::shell_path::decode_literal_word(name);
        if !name.starts_with(['-', '+']) {
            return None;
        }
        let mut exact = None;
        for option in options {
            if let Some(spelling) = option
                .short
                .iter()
                .chain(&option.long)
                .find(|s| **s == name)
            {
                exact = Some(Self {
                    name: spelling,
                    option,
                    attached,
                });
                // A subcommand may add a value to a global flag spelling.
                // Preserve the first declared value type in the active scope.
                if option.expects_value() {
                    break;
                }
            }
        }
        if let Some(word) = exact {
            record(word.name);
            return Some(word);
        }

        // Exact names like ffmpeg's "-codec" win over "-c" with a value.
        // Read unquoted ASCII flags, keeping the value in raw source coordinates.
        let mut flags = raw.strip_prefix('-')?;
        if flags.starts_with('-') {
            return None;
        }
        while let Some((flag, rest)) = flags.split_at_checked(1) {
            let (option, name) = options.iter().find_map(|option| {
                let name = option.short.as_deref()?;
                (name.strip_prefix('-') == Some(flag)).then_some((*option, name))
            })?;
            record(name);
            if rest.is_empty() || option.expects_value() {
                if rest.starts_with('=') {
                    return None;
                }
                return Some(Self {
                    name,
                    option,
                    attached: (!rest.is_empty()).then_some(rest),
                });
            }
            flags = rest;
        }
        None
    }

    pub(crate) fn takes_separate_value(&self) -> bool {
        self.attached.is_none() && self.option.takes_separate_value()
    }

    pub(crate) fn can_extend_cluster(&self) -> bool {
        !self.option.expects_value() && self.attached.is_none() && self.name.len() == 2
    }

    // pacman's operation letter is a subcommand (e.g. -Q in -Qb), so
    // ordinary short-option lookup alone cannot recognize these path values.
    fn for_cursor(raw: &'raw str, options: &[&'opt CommandOption], command: &str) -> Option<Self> {
        Self::parse(raw, options).or_else(|| {
            if command != "pacman" {
                return None;
            }
            let (name, path) = pacman_short_path(raw)?;
            let option = options.iter().find(|opt| opt.matches_name(name))?;
            Some(Self {
                name,
                option,
                attached: (!path.is_empty()).then_some(path),
            })
        })
    }
}

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
            let logical = super::super::shell_path::decode_literal_word(token);
            if !end_options && logical == "--" {
                end_options = true;
                index += 1;
                continue;
            }
            if !end_options && let Some(word) = OptionWord::parse(&logical, options) {
                index += 1;
                if word.takes_separate_value() {
                    index = Self::next_argv_index(&parsed.raw_args, index) + 1;
                }
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
        let mut index = Self::next_argv_index(&parsed.raw_args, 0);
        while index < current {
            let token = super::super::shell_path::decode_literal_word(parsed.raw_args.get(index)?);
            if token == "--" {
                return None;
            }
            index = Self::next_argv_index(&parsed.raw_args, index + 1);
            let Some(word) = OptionWord::for_cursor(&token, &options, &parsed.command) else {
                continue;
            };
            if word.takes_separate_value() {
                if index == current {
                    let mut corrected = parsed.clone();
                    if let Some(raw) = Self::current_raw_token(parsed) {
                        corrected.current_token = raw.to_owned();
                        corrected.current_arg = Some(raw.to_owned());
                    }
                    corrected.completion_context = CompletionContext::OptionValue {
                        option_name: word.name.to_owned(),
                        value_type: word.option.value_type().cloned(),
                    };
                    Self::remove_current_value_from_arguments(&mut corrected);
                    return Some(corrected);
                }
                index = Self::next_argv_index(&parsed.raw_args, index + 1);
            } else if index == current
                && word.option.value_attached_only
                && matches!(&parsed.completion_context, CompletionContext::OptionValue { option_name, .. } if option_name == word.name)
                && Self::current_raw_token(parsed) == Some(parsed.current_token.as_str())
            {
                let mut corrected = parsed.clone();
                let (_, arg_index) =
                    Self::positional_arguments_after_raw_index(parsed, 0, &options);
                corrected.completion_context = CompletionContext::Argument {
                    arg_index,
                    arg_type: None,
                };
                return Some(corrected);
            }
        }

        if let Some(raw) = Self::current_raw_token(parsed)
            && let Some(word) = OptionWord::for_cursor(raw, &options, &parsed.command)
            && let Some(value) = word.attached
        {
            if !word.option.expects_value() {
                let mut corrected = parsed.clone();
                corrected.current_token = raw.to_owned();
                corrected.current_arg = Some(raw.to_owned());
                corrected.completion_context = CompletionContext::LongOption;
                return Some(corrected);
            }

            // While editing, "-ver" can still become ffmpeg's "-version".
            if !word.name.starts_with("--") {
                let prefix = super::super::shell_path::decode_literal_word(raw);
                if options.iter().any(|opt| {
                    opt.short
                        .iter()
                        .chain(&opt.long)
                        .any(|s| s.starts_with(&prefix))
                }) {
                    return None;
                }
            }
            let mut corrected = parsed.clone();
            corrected.current_token = value.to_owned();
            corrected.current_arg = Some(value.to_owned());
            corrected.completion_context = CompletionContext::OptionValue {
                option_name: word.name.to_owned(),
                value_type: word.option.value_type().cloned(),
            };
            corrected.specified_options =
                normalize_specified_option(&corrected.specified_options, raw, word.name);
            corrected.options = corrected.specified_options.clone();
            return Some(corrected);
        }

        None
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

            if !end_of_options && super::super::shell_path::decode_literal_word(token) == "--" {
                end_of_options = true;
                raw_index += 1;
                continue;
            }

            if !end_of_options && let Some(word) = OptionWord::parse(token, options) {
                raw_index += 1;
                if word.takes_separate_value()
                    && let Some(value) = parsed.raw_args.get(raw_index)
                    && OptionWord::parse(value, options).is_none()
                {
                    if parsed.specified_arguments.get(specified_index) == Some(value) {
                        specified_index += 1;
                    }
                    raw_index += 1;
                }
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
