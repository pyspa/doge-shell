use crate::completion::command::{ArgumentType, CommandCompletionDatabase, CompletionCandidate};
use crate::completion::errors::GeneratorError;
use crate::completion::fuzzy_match_score;
use crate::completion::generators::subcommand::SubCommandGenerator;
use crate::completion::generators::system::{
    SystemCommandCacheTicket, SystemCommandGenerator, activate_system_command_cache,
};
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

pub struct CommandGenerator<'a> {
    database: &'a CommandCompletionDatabase,
    environment_names: &'a [String],
    system_command_ticket: Option<SystemCommandCacheTicket>,
}

impl<'a> CommandGenerator<'a> {
    pub fn new(database: &'a CommandCompletionDatabase) -> Self {
        Self {
            database,
            environment_names: &[],
            system_command_ticket: None,
        }
    }

    pub fn with_runtime_environment(
        database: &'a CommandCompletionDatabase,
        environment_names: &'a [String],
        system_paths: &'a [String],
    ) -> Self {
        Self::with_runtime_environment_ticket(
            database,
            environment_names,
            Some(activate_system_command_cache(system_paths)),
        )
    }

    pub(crate) fn with_runtime_environment_ticket(
        database: &'a CommandCompletionDatabase,
        environment_names: &'a [String],
        system_command_ticket: Option<SystemCommandCacheTicket>,
    ) -> Self {
        Self {
            database,
            environment_names,
            system_command_ticket,
        }
    }

    /// Generate command name completion candidates
    pub fn generate_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let mut candidates = Vec::with_capacity(32);

        // Commands registered in database
        for (command_name, completion) in self.database.iter_commands() {
            if fuzzy_match_score(command_name, current_token).is_some() {
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
        Ok(self
            .system_command_ticket
            .as_ref()
            .map(|ticket| {
                SystemCommandGenerator::from_activation(ticket.clone())
                    .generate_candidates(current_token)
            })
            .transpose()?
            .unwrap_or_default())
    }

    /// Generate environment variable completion candidates from injected
    /// runtime names. No `std::env` fallback: without injected names the
    /// list is empty.
    pub fn generate_environment_variable_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(32);

        for key in self.environment_names {
            if fuzzy_match_score(key, current_token).is_some() {
                candidates.push(CompletionCandidate::argument(key.clone(), None));
            }
        }

        Ok(candidates)
    }
}
