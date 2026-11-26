use crate::completion::command::{
    CommandCompletion, CommandOption, CompletionCandidate, ShortOptionClusters,
};
use crate::completion::context::{OptionWord, scan_scoped_options};
use crate::completion::integrated::matches_prefix;
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

pub struct OptionGenerator;

impl OptionGenerator {
    pub fn generate_short_option_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let options = command_completion.option_candidates_for_path(&parsed.subcommand_path);
        Ok(Self::match_options(
            &options,
            parsed,
            false,
            Some(command_completion),
        ))
    }

    pub fn generate_long_option_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let options = command_completion.option_candidates_for_path(&parsed.subcommand_path);
        // Long-option context also offers short options because the parser
        // treats a single "-" as LongOption context.
        Ok(Self::match_options(
            &options,
            parsed,
            true,
            Some(command_completion),
        ))
    }

    /// Match options with **prefix-first, fuzzy-fallback** semantics (same as
    /// subcommands): prefix matches win when present, fuzzy only fills in when
    /// nothing prefix-matches (so `git commit --mesage` can still reach
    /// `--message`). Already-specified options are excluded.
    fn match_options(
        options: &[&CommandOption],
        parsed: &ParsedCommandLine,
        include_long: bool,
        definition: Option<&CommandCompletion>,
    ) -> Vec<CompletionCandidate> {
        let clusters = definition.and_then(|d| d.short_option_clusters.as_ref());
        // The parser records the token currently under the cursor in
        // `specified_options` too. Drop one occurrence of it so that completing
        // a fully-typed option (e.g. `--all`) is not self-excluded just because
        // it prefixes another option (e.g. `--all-match`).
        let query = parsed.completion_query();
        let mut already_specified = Vec::new();
        let scoped = definition.filter(|d| d.has_scoped_options_on_path(&parsed.subcommand_path));
        if let (Some(d), Some(source)) = (scoped, &parsed.source) {
            for (index, owner, name) in scan_scoped_options(d, source).used {
                if index != source.cursor_word
                    && options.iter().any(|&opt| std::ptr::eq(opt, owner))
                {
                    already_specified.push(name);
                }
            }
        } else if clusters.is_some_and(ShortOptionClusters::enabled)
            && let Some(source) = &parsed.source
        {
            let mut argv = source.argv();
            while let Some((index, raw)) = argv.next() {
                let logical = crate::completion::shell_path::decode_literal_word(raw);
                if logical == "--" && index != source.cursor_word {
                    break;
                }
                let before = already_specified.len();
                let word = OptionWord::visit(&logical, options, |_, name| {
                    if index != source.cursor_word {
                        already_specified.push(name);
                    }
                });
                match word {
                    Some(word) if word.takes_separate_value() => {
                        argv.next();
                    }
                    None => already_specified.truncate(before),
                    _ => {}
                }
            }
        } else {
            already_specified.extend(parsed.specified_options.iter().map(String::as_str));
            if let Some(pos) = already_specified.iter().position(|&opt| opt == query) {
                already_specified.remove(pos);
            }
        }
        let used = |option: &CommandOption, name: &str| {
            !option.multiple
                && already_specified.iter().any(|&specified| {
                    specified == name
                        || (option.matches_name(specified)
                            && options
                                .iter()
                                .filter(|other| other.matches_name(specified))
                                .count()
                                == 1)
                })
        };

        let build = |matcher: &dyn Fn(&str, &str) -> bool| -> Vec<CompletionCandidate> {
            let mut candidates = Vec::with_capacity(16);
            for option in options {
                // Global and subcommand options may reuse a short name
                // (git -p: paginate versus fetch --prune). An ambiguous
                // spelling cannot prove that its other name was used.
                if let Some(ref long) = option.long
                    && (include_long || (long.starts_with('-') && !long.starts_with("--")))
                    && matcher(&query, long)
                    && !used(option, long)
                {
                    candidates.push(CompletionCandidate::long_option(
                        long.clone(),
                        option.description.clone(),
                    ));
                }

                if let Some(ref short) = option.short
                    && matcher(&query, short)
                    && !used(option, short)
                {
                    candidates.push(CompletionCandidate::short_option(
                        short.clone(),
                        option.description.clone(),
                    ));
                }
            }
            candidates
        };

        let mut prefix = build(&|token: &str, value: &str| value.starts_with(token));
        if let Some(clusters) = clusters
            && let Some(flags) = query.strip_prefix('-')
            && !flags.is_empty()
            && flags.chars().all(|ch| clusters.allows(ch))
            && OptionWord::visit(&query, options, |_, _| {})
                .is_some_and(|word| word.can_extend_cluster())
        {
            for option in options {
                if let Some(short) = option.short.as_deref()
                    && let Some(letter) = short.strip_prefix('-')
                    && letter.len() == 1
                    && clusters.allows(char::from(letter.as_bytes()[0]))
                    && (option.multiple || !flags.contains(letter))
                    && !used(option, short)
                {
                    prefix.push(CompletionCandidate::short_option(
                        format!("{query}{letter}"),
                        option.description.clone(),
                    ));
                }
            }
        }
        if !prefix.is_empty() {
            return prefix;
        }
        build(&|token: &str, value: &str| matches_prefix(token, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CompletionContext;

    fn opt(long: &str, description: &str) -> CommandOption {
        CommandOption {
            multiple: false,
            short: None,
            long: Some(long.to_string()),
            description: Some(description.to_string()),
            inherit: None,
            value_attached_only: false,
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
            source: None,
        }
    }

    fn long_texts(options: &[CommandOption], token: &str) -> Vec<String> {
        let refs: Vec<&CommandOption> = options.iter().collect();
        OptionGenerator::match_options(&refs, &parsed(token), true, None)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn multiple_options_remain_available_under_both_names() {
        let option: CommandOption =
            serde_json::from_str(r#"{"short":"-e","long":"--regexp","multiple":true}"#).unwrap();
        for used in ["-e", "--regexp"] {
            let mut p = parsed("-");
            p.specified_options = vec![used.into()];
            let candidates = OptionGenerator::match_options(&[&option], &p, true, None);
            assert_eq!(
                candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["--regexp", "-e"]
            );
        }
    }

    #[test]
    fn single_use_option_excludes_its_other_spelling() {
        let option: CommandOption =
            serde_json::from_str(r#"{"short":"-i","long":"--ignore-case"}"#).unwrap();
        for used in ["-i", "--ignore-case"] {
            let mut p = parsed("-");
            p.specified_options = vec![used.into()];
            assert!(OptionGenerator::match_options(&[&option], &p, true, None).is_empty());
        }
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
        let options = [opt("--all", "a"), opt("--all-match", "am")];
        let mut p = parsed("--all");
        p.specified_options = vec!["--all".to_string()];
        let refs: Vec<&CommandOption> = options.iter().collect();
        let got: Vec<String> = OptionGenerator::match_options(&refs, &p, true, None)
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
        let options = [opt("--foo", "f"), opt("--bar", "b")];
        let mut p = parsed("--");
        p.specified_options = vec!["--foo".to_string(), "--".to_string()];
        let refs: Vec<&CommandOption> = options.iter().collect();
        let got: Vec<String> = OptionGenerator::match_options(&refs, &p, true, None)
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(!got.contains(&"--foo".to_string()), "got: {got:?}");
        assert!(got.contains(&"--bar".to_string()), "got: {got:?}");
    }
}
