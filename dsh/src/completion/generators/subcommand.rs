use crate::completion::command::{
    ArgumentType, CommandCompletion, CompletionCandidate, SubCommand,
};
use crate::completion::generators::filesystem::FileSystemGenerator;
use crate::completion::integrated::matches_prefix;
use crate::completion::parser::ParsedCommandLine; // Circular dependency if we call back?
// Actually SubCommandGenerator needs access to `generate_candidates_for_type` which is currently in CompletionGenerator.
// Ideally, `generate_candidates_for_type` should be a shared utility or trait.
// For now, let's duplicate or move `generate_candidates_for_type` to a shared place?
// Or, SubCommandGenerator shouldn't depend on CompletionGenerator.
// It generates arguments too.
use anyhow::Result;

pub struct SubCommandGenerator;

impl SubCommandGenerator {
    pub fn generate_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
        // We need a callback or helper for arguments
        arg_generator_fn: impl Fn(&ArgumentType, &ParsedCommandLine) -> Result<Vec<CompletionCandidate>>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);
        let current_subcommand =
            Self::find_current_subcommand(command_completion, &parsed.subcommand_path);

        if let Some(subcommand) = current_subcommand {
            // Nested subcommand candidates
            candidates.extend(Self::match_subcommands(
                &subcommand.subcommands,
                &parsed.current_token,
            ));
        } else {
            // Match subcommands
            candidates.extend(Self::match_subcommands(
                &command_completion.subcommands,
                &parsed.current_token,
            ));

            // Check if we should suggest arguments
            let arg_index = parsed.specified_arguments.len();
            if arg_index < command_completion.arguments.len() {
                let arg_def = &command_completion.arguments[arg_index];
                let arg_candidates = arg_generator_fn(
                    arg_def.arg_type.as_ref().unwrap_or(&ArgumentType::String),
                    parsed,
                )?;
                candidates.extend(arg_candidates);
            }
        }

        if candidates.is_empty() {
            // Fallback for unknown commands OR valid commands with undefined arguments
            // This ensures `git add <file>` works even if we have some minimal git definition
            candidates.extend(FileSystemGenerator::generate_file_candidates(
                &parsed.current_token,
            )?);
        }

        Ok(candidates)
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

    /// Match subcommands with **prefix-first, fuzzy-fallback** semantics: when
    /// any subcommand prefix-matches the token we return only those (so `git a`
    /// stays scoped to add/am/apply/…); only when nothing prefix-matches do we
    /// fall back to fuzzy matching (so `git ct` can still reach `commit`).
    fn match_subcommands(
        subcommands: &[SubCommand],
        current_token: &str,
    ) -> Vec<CompletionCandidate> {
        let build = |matcher: &dyn Fn(&str, &str) -> bool| -> Vec<CompletionCandidate> {
            subcommands
                .iter()
                .filter_map(|sc| {
                    Self::matched_subcommand_text(sc, current_token, matcher)
                        .map(|text| CompletionCandidate::subcommand(text, sc.description.clone()))
                })
                .collect()
        };

        let prefix = build(&|token: &str, value: &str| value.starts_with(token));
        if !prefix.is_empty() {
            return prefix;
        }
        // No prefix match anywhere: allow fuzzy matches.
        build(&|token: &str, value: &str| matches_prefix(token, value))
    }

    fn matched_subcommand_text(
        subcommand: &SubCommand,
        current_token: &str,
        matcher: &dyn Fn(&str, &str) -> bool,
    ) -> Option<String> {
        if matcher(current_token, &subcommand.name) {
            return Some(subcommand.name.clone());
        }

        subcommand
            .aliases
            .iter()
            .find(|alias| matcher(current_token, alias))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(name: &str) -> SubCommand {
        SubCommand {
            name: name.to_string(),
            description: None,
            aliases: vec![],
            options: vec![],
            arguments: vec![],
            subcommands: vec![],
        }
    }

    fn texts(subs: &[SubCommand], token: &str) -> Vec<String> {
        SubCommandGenerator::match_subcommands(subs, token)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn prefix_matches_win_and_exclude_fuzzy_noise() {
        let subs = vec![sub("add"), sub("am"), sub("commit"), sub("branch")];
        let got = texts(&subs, "a");
        // Only prefix `a*` — not `branch`/`commit` (which merely contain 'a').
        assert!(got.contains(&"add".to_string()));
        assert!(got.contains(&"am".to_string()));
        assert!(!got.contains(&"branch".to_string()), "got: {got:?}");
        assert!(!got.contains(&"commit".to_string()), "got: {got:?}");
    }

    #[test]
    fn fuzzy_fallback_when_no_prefix_match() {
        let subs = vec![sub("add"), sub("commit"), sub("checkout")];
        let got = texts(&subs, "ct");
        // Nothing starts with "ct" → fuzzy fallback reaches `commit`.
        assert!(got.contains(&"commit".to_string()), "got: {got:?}");
    }

    #[test]
    fn empty_token_returns_all_via_prefix() {
        let subs = vec![sub("add"), sub("commit")];
        assert_eq!(texts(&subs, "").len(), 2);
    }
}
