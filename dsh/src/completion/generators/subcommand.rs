use crate::completion::command::{
    ArgumentType, CommandCompletion, CompletionCandidate, SubCommand,
};
use crate::completion::generators::filesystem::FileSystemGenerator;
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
            for sub in &subcommand.subcommands {
                if let Some(text) = Self::matched_subcommand_text(sub, &parsed.current_token) {
                    candidates.push(CompletionCandidate::subcommand(
                        text,
                        sub.description.clone(),
                    ));
                }
            }
        } else {
            // Match subcommands
            for subcommand in &command_completion.subcommands {
                if let Some(text) = Self::matched_subcommand_text(subcommand, &parsed.current_token)
                {
                    candidates.push(CompletionCandidate::subcommand(
                        text,
                        subcommand.description.clone(),
                    ));
                }
            }

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

            if let Some(sc) = current_subcommand {
                current_subcommands = &sc.subcommands;
            } else {
                return None;
            }
        }

        current_subcommand
    }

    fn matched_subcommand_text(subcommand: &SubCommand, current_token: &str) -> Option<String> {
        if subcommand.name.starts_with(current_token) {
            return Some(subcommand.name.clone());
        }

        subcommand
            .aliases
            .iter()
            .find(|alias| alias.starts_with(current_token))
            .cloned()
    }
}
