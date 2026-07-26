use crate::completion::command::{
    CommandCompletion, CommandOption, CompletionCandidate, SubCommand,
};
use crate::completion::integrated::matches_prefix;
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

pub struct OptionGenerator;

impl OptionGenerator {
    pub fn generate_short_option_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let options = Self::collect_available_options(command_completion, &parsed.subcommand_path);
        Ok(Self::match_options(&options, parsed, false, true))
    }

    pub fn generate_long_option_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let options = Self::collect_available_options(command_completion, &parsed.subcommand_path);
        // Long-option context also offers short options because the parser
        // treats a single "-" as LongOption context.
        Ok(Self::match_options(&options, parsed, true, true))
    }

    /// Match options with **prefix-first, fuzzy-fallback** semantics (same as
    /// subcommands): prefix matches win when present, fuzzy only fills in when
    /// nothing prefix-matches (so `git commit --mesage` can still reach
    /// `--message`). Already-specified options are excluded.
    fn match_options(
        options: &[&CommandOption],
        parsed: &ParsedCommandLine,
        include_long: bool,
        include_short: bool,
    ) -> Vec<CompletionCandidate> {
        // The parser records the token currently under the cursor in
        // `specified_options` too. Drop one occurrence of it so that completing
        // a fully-typed option (e.g. `--all`) is not self-excluded just because
        // it prefixes another option (e.g. `--all-match`).
        let mut already_specified = parsed.specified_options.clone();
        if let Some(pos) = already_specified
            .iter()
            .position(|opt| opt == &parsed.current_token)
        {
            already_specified.remove(pos);
        }

        let build = |matcher: &dyn Fn(&str, &str) -> bool| -> Vec<CompletionCandidate> {
            let mut candidates = Vec::with_capacity(16);
            for option in options {
                if include_long
                    && let Some(ref long) = option.long
                    && matcher(&parsed.current_token, long)
                    && !already_specified.contains(long)
                {
                    candidates.push(CompletionCandidate::long_option(
                        long.clone(),
                        option.description.clone(),
                    ));
                }

                if include_short
                    && let Some(ref short) = option.short
                    && matcher(&parsed.current_token, short)
                    && !already_specified.contains(short)
                {
                    candidates.push(CompletionCandidate::short_option(
                        short.clone(),
                        option.description.clone(),
                    ));
                }
            }
            candidates
        };

        let prefix = build(&|token: &str, value: &str| value.starts_with(token));
        if !prefix.is_empty() {
            return prefix;
        }
        build(&|token: &str, value: &str| matches_prefix(token, value))
    }

    fn collect_available_options<'b>(
        command_completion: &'b CommandCompletion,
        subcommand_path: &[String],
    ) -> Vec<&'b CommandOption> {
        let mut options = Vec::new();

        // Global options
        options.extend(&command_completion.global_options);

        // Subcommand options
        if let Some(subcommand) = Self::find_current_subcommand(command_completion, subcommand_path)
        {
            options.extend(&subcommand.options);
        }

        options
    }

    fn find_current_subcommand<'b>(
        command_completion: &'b CommandCompletion,
        subcommand_path: &[String],
    ) -> Option<&'b SubCommand> {
        if subcommand_path.is_empty() {
            return None;
        }

        let mut current_subcommands = &command_completion.subcommands;
        let mut current_subcommand = None;

        for subcommand_name in subcommand_path {
            current_subcommand = current_subcommands
                .iter()
                .find(|sc| sc.name == *subcommand_name || sc.aliases.contains(subcommand_name));

            {
                let sc = current_subcommand?;
                current_subcommands = &sc.subcommands;
            }
        }

        current_subcommand
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CompletionContext;

    fn opt(long: &str, description: &str) -> CommandOption {
        CommandOption {
            short: None,
            long: Some(long.to_string()),
            description: Some(description.to_string()),
            takes_value: false,
            value_type: None,
            argument: None,
        }
    }

    fn parsed(token: &str) -> ParsedCommandLine {
        ParsedCommandLine {
            command: "demo".to_string(),
            subcommand_path: vec![],
            raw_args: vec![],
            args: vec![],
            options: vec![],
            current_token: token.to_string(),
            current_arg: Some(token.to_string()),
            completion_context: CompletionContext::LongOption,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }
    }

    fn long_texts(options: &[CommandOption], token: &str) -> Vec<String> {
        let refs: Vec<&CommandOption> = options.iter().collect();
        OptionGenerator::match_options(&refs, &parsed(token), true, true)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn prefix_options_win_over_fuzzy() {
        let options = vec![
            opt("--verbose", "v"),
            opt("--version", "V"),
            opt("--all", "a"),
        ];
        let got = long_texts(&options, "--ver");
        assert!(got.contains(&"--verbose".to_string()));
        assert!(got.contains(&"--version".to_string()));
        assert!(!got.contains(&"--all".to_string()), "got: {got:?}");
    }

    #[test]
    fn option_fuzzy_fallback_on_typo() {
        let options = vec![opt("--message", "m"), opt("--amend", "a")];
        // `--mesage` (typo) prefix-matches nothing → fuzzy fallback → `--message`.
        let got = long_texts(&options, "--mesage");
        assert!(got.contains(&"--message".to_string()), "got: {got:?}");
    }

    #[test]
    fn current_token_option_is_not_self_excluded() {
        // `--all` fully typed, TAB pressed. The parser records `--all` in
        // specified_options; it must NOT be excluded from its own completion
        // even though it prefixes `--all-match`.
        let options = vec![opt("--all", "a"), opt("--all-match", "am")];
        let mut p = parsed("--all");
        p.specified_options = vec!["--all".to_string()];
        let refs: Vec<&CommandOption> = options.iter().collect();
        let got: Vec<String> = OptionGenerator::match_options(&refs, &p, true, true)
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(got.contains(&"--all".to_string()), "got: {got:?}");
        assert!(got.contains(&"--all-match".to_string()), "got: {got:?}");
    }

    #[test]
    fn previously_specified_option_is_still_excluded() {
        // `--foo` was specified earlier; now completing a fresh `--` token.
        // `--foo` must be excluded, `--bar` offered.
        let options = vec![opt("--foo", "f"), opt("--bar", "b")];
        let mut p = parsed("--");
        p.specified_options = vec!["--foo".to_string(), "--".to_string()];
        let refs: Vec<&CommandOption> = options.iter().collect();
        let got: Vec<String> = OptionGenerator::match_options(&refs, &p, true, true)
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(!got.contains(&"--foo".to_string()), "got: {got:?}");
        assert!(got.contains(&"--bar".to_string()), "got: {got:?}");
    }
}
