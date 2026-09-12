use super::command::{CommandCompletionDatabase, CompletionCandidate};
use super::context::ContextCorrector;
pub use super::errors::GeneratorError;
use super::generators::argument::ArgumentGenerator;
use super::generators::command::CommandGenerator;
use super::parser::{CompletionContext, ParsedCommandLine};
use anyhow::Result;

// Re-export for compatibility
pub use super::generators::system::{clear_global_system_commands, set_global_system_commands};

/// Completion candidate generator
pub struct CompletionGenerator<'a> {
    /// Command completion database
    database: &'a CommandCompletionDatabase,
}

impl<'a> CompletionGenerator<'a> {
    /// Create a new generator
    pub fn new(database: &'a CommandCompletionDatabase) -> Self {
        Self { database }
    }

    /// Get available command list (for debugging)
    pub fn get_available_commands(&self) -> Vec<String> {
        self.database
            .get_command_names()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Check if a command has JSON completion data available
    pub fn has_command_completion(&self, command: &str) -> bool {
        self.database.get_command(command).is_some()
    }

    /// Public fallback generator (Files + System)
    pub fn generate_fallback_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let mut candidates = crate::completion::generators::filesystem::FileSystemGenerator::generate_file_candidates(current_token)
            .map_err(GeneratorError::Other)?;

        candidates.extend(
            crate::completion::generators::system::SystemCommandGenerator::new()
                .generate_candidates(current_token)
                .map_err(|e| GeneratorError::Other(anyhow::anyhow!(e)))?,
        );

        Ok(candidates)
    }

    /// Generate completion candidates from parsed command line
    ///
    /// This method also corrects the parsed command line if the parser
    /// incorrectly identified arguments as subcommands.
    pub fn correct_parsed_command_line(&self, parsed: &ParsedCommandLine) -> ParsedCommandLine {
        ContextCorrector::new(self.database).correct_parsed_command_line(parsed)
    }

    /// Generate completion candidates from parsed command line
    pub fn generate_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        self.generate_candidates_impl(parsed)
    }

    fn generate_candidates_impl(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let corrected = self.correct_parsed_command_line(parsed);

        match &corrected.completion_context {
            CompletionContext::Command => CommandGenerator::new(self.database)
                .generate_command_candidates(&corrected.current_token),
            CompletionContext::SubCommand => CommandGenerator::new(self.database)
                .generate_subcommand_candidates(&corrected, |arg_type, p| {
                    ArgumentGenerator::new(self.database).generate_candidates_for_type(arg_type, p)
                }),
            CompletionContext::ShortOption => ArgumentGenerator::new(self.database)
                .generate_short_option_candidates(&corrected, |p| self.generate_candidates(p)),
            CompletionContext::LongOption => ArgumentGenerator::new(self.database)
                .generate_long_option_candidates(&corrected, |p| self.generate_candidates(p)),
            CompletionContext::OptionValue {
                option_name: _,
                value_type,
            } => ArgumentGenerator::new(self.database)
                .generate_option_value_candidates(&corrected, value_type.as_ref()),
            CompletionContext::Argument {
                arg_index: _,
                arg_type,
            } => ArgumentGenerator::new(self.database).generate_argument_candidates(
                &corrected,
                arg_type.as_ref(),
                |p| self.generate_candidates(p),
            ),
            CompletionContext::Unknown => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests;
