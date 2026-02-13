use crate::completion::command::{ArgumentType, CommandCompletionDatabase, CompletionCandidate};
use crate::completion::errors::GeneratorError;
use crate::completion::fuzzy_match_score;
use crate::completion::generators::subcommand::SubCommandGenerator;
use crate::completion::generators::system::SystemCommandGenerator;
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

pub struct CommandGenerator<'a> {
    database: &'a CommandCompletionDatabase,
}

impl<'a> CommandGenerator<'a> {
    pub fn new(database: &'a CommandCompletionDatabase) -> Self {
        Self { database }
    }

    /// Generate command name completion candidates
    pub fn generate_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let mut candidates = Vec::with_capacity(32);

        // Commands registered in database
        for command_name in self.database.get_command_names() {
            if fuzzy_match_score(command_name, current_token).is_some()
                && let Some(completion) = self.database.get_command(command_name)
            {
                candidates.push(CompletionCandidate::subcommand(
                    command_name.clone(),
                    completion.description.clone(),
                ));
            }
        }

        // Also add system commands (simplified version)
        candidates.extend(self.generate_system_command_candidates(current_token)?);

        Ok(candidates)
    }

    /// Generate subcommand completion candidates
    pub fn generate_subcommand_candidates<F>(
        &self,
        parsed: &ParsedCommandLine,
        generate_args: F,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError>
    where
        F: Fn(&ArgumentType, &ParsedCommandLine) -> Result<Vec<CompletionCandidate>>,
    {
        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            SubCommandGenerator::generate_candidates(command_completion, parsed, generate_args)
                .map_err(GeneratorError::Other)
        } else {
            // Signal missing command so the engine can try to load it
            Err(GeneratorError::MissingCommand(parsed.command.clone()))
        }
    }

    /// Generate system command completion candidates (simplified version)
    pub fn generate_system_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        SystemCommandGenerator::new().generate_candidates(current_token)
    }

    /// Generate environment variable completion candidates
    pub fn generate_environment_variable_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(32);

        for (key, _) in std::env::vars() {
            if fuzzy_match_score(&key, current_token).is_some() {
                candidates.push(CompletionCandidate::argument(key, None));
            }
        }

        Ok(candidates)
    }
}
