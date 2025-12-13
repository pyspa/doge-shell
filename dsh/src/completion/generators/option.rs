use crate::completion::command::{
    CommandCompletion, CommandOption, CompletionCandidate, SubCommand,
};
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

pub struct OptionGenerator;

impl OptionGenerator {
    pub fn generate_short_option_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);
        let options = Self::collect_available_options(command_completion, &parsed.subcommand_path);

        for option in options {
            if let Some(ref short) = option.short
                && short.starts_with(&parsed.current_token)
                && !parsed.specified_options.contains(short)
            {
                candidates.push(CompletionCandidate::short_option(
                    short.clone(),
                    option.description.clone(),
                ));
            }
        }

        Ok(candidates)
    }

    pub fn generate_long_option_candidates(
        command_completion: &CommandCompletion,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);
        let options = Self::collect_available_options(command_completion, &parsed.subcommand_path);

        for option in options {
            if let Some(ref long) = option.long
                && long.starts_with(&parsed.current_token)
                && !parsed.specified_options.contains(long)
            {
                candidates.push(CompletionCandidate::long_option(
                    long.clone(),
                    option.description.clone(),
                ));
            }

            // Also check short options because the parser treats single "-" as LongOption context
            if let Some(ref short) = option.short
                && short.starts_with(&parsed.current_token)
                && !parsed.specified_options.contains(short)
            {
                candidates.push(CompletionCandidate::short_option(
                    short.clone(),
                    option.description.clone(),
                ));
            }
        }

        Ok(candidates)
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
                .find(|sc| sc.name == *subcommand_name);

            if let Some(sc) = current_subcommand {
                current_subcommands = &sc.subcommands;
            } else {
                return None;
            }
        }

        current_subcommand
    }
}
