use crate::completion::command::{ArgumentType, CommandCompletionDatabase, CompletionCandidate};
use crate::completion::context::ContextCorrector;
use crate::completion::errors::GeneratorError;
use crate::completion::fuzzy_match_score;
use crate::completion::parser::{CompletionContext, ParsedCommandLine};

use super::filesystem::FileSystemGenerator;
use super::group::GroupGenerator;
use super::interface::InterfaceGenerator;
use super::option::OptionGenerator;
use super::process::ProcessGenerator;
use super::script::ScriptGenerator;
use super::signal::SignalGenerator;
use super::system::SystemCommandGenerator;
use super::user::UserGenerator;

use anyhow::Result;

pub struct ArgumentGenerator<'a> {
    database: &'a CommandCompletionDatabase,
}

impl<'a> ArgumentGenerator<'a> {
    pub fn new(database: &'a CommandCompletionDatabase) -> Self {
        Self { database }
    }

    /// Generate argument completion candidates
    pub fn generate_argument_candidates<F>(
        &self,
        parsed: &ParsedCommandLine,
        arg_type: Option<&ArgumentType>,
        recurse_cb: F,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError>
    where
        F: Fn(&ParsedCommandLine) -> Result<Vec<CompletionCandidate>, GeneratorError>,
    {
        let mut candidates = Vec::new();

        // Get actual argument type
        let actual_arg_type = arg_type;

        if let Some(arg_type) = actual_arg_type {
            candidates.extend(self.generate_candidates_for_type(arg_type, parsed)?);
        } else {
            // Try to resolve argument type from database if not specified in context
            if let Some(command_completion) = self.database.get_command(&parsed.command)
                && let CompletionContext::Argument { arg_index, .. } = parsed.completion_context
            {
                let mut current_arguments = &command_completion.arguments;
                let mut current_subcommands = &command_completion.subcommands;

                for sub_name in &parsed.subcommand_path {
                    if let Some(sub) = current_subcommands.iter().find(|s| &s.name == sub_name) {
                        current_arguments = &sub.arguments;
                        current_subcommands = &sub.subcommands;
                    } else {
                        break;
                    }
                }

                if let Some(arg_def) = current_arguments.get(arg_index)
                    && let Some(ref arg_type) = arg_def.arg_type
                {
                    candidates.extend(self.generate_candidates_for_type(arg_type, parsed)?);
                }
            }

            // Default to file completion if no candidates generated yet
            if candidates.is_empty() {
                candidates.extend(
                    FileSystemGenerator::generate_file_candidates(&parsed.current_token)
                        .map_err(GeneratorError::Other)?,
                );
            }
        }

        let corrector = ContextCorrector::new(self.database);
        if let Some((cmd_arg_index, cmd_name)) = corrector.find_command_with_args_arg(parsed)
            && let CompletionContext::Argument { arg_index, .. } = parsed.completion_context
            && arg_index > cmd_arg_index
        {
            // Recursive completion for the inner command
            let inner_parsed = corrector.reparse_inner_command(parsed, cmd_arg_index, cmd_name);
            return recurse_cb(&inner_parsed);
        }

        Ok(candidates)
    }

    /// Generate option value completion candidates
    pub fn generate_option_value_candidates(
        &self,
        parsed: &ParsedCommandLine,
        value_type: Option<&ArgumentType>,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let mut candidates = Vec::new();

        // Get actual value type
        let mut actual_value_type = value_type;

        // If type is not provided by context, look it up in the database
        if actual_value_type.is_none()
            && let Some(command_completion) = self.database.get_command(&parsed.command)
            && let CompletionContext::OptionValue {
                ref option_name, ..
            } = parsed.completion_context
        {
            let mut options = Vec::new();
            options.extend(&command_completion.global_options);

            // Find current subcommand to get its options
            let mut current_subcommands = &command_completion.subcommands;
            for subcommand_name in &parsed.subcommand_path {
                if let Some(sc) = current_subcommands
                    .iter()
                    .find(|s| &s.name == subcommand_name)
                {
                    options.extend(&sc.options);
                    current_subcommands = &sc.subcommands;
                } else {
                    break;
                }
            }

            if let Some(opt) = options.iter().find(|o| {
                o.short.as_ref() == Some(option_name) || o.long.as_ref() == Some(option_name)
            }) && let Some(ref arg) = opt.argument
            {
                actual_value_type = arg.arg_type.as_ref();
            }
        }

        if let Some(arg_type) = actual_value_type {
            candidates.extend(self.generate_candidates_for_type(arg_type, parsed)?);
        }

        // Fallback: file completion
        if candidates.is_empty() {
            candidates.extend(
                FileSystemGenerator::generate_file_candidates(&parsed.current_token)
                    .map_err(GeneratorError::Other)?,
            );
        }

        Ok(candidates)
    }

    /// Generate short option completion candidates
    pub fn generate_short_option_candidates<F>(
        &self,
        parsed: &ParsedCommandLine,
        recurse_cb: F,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError>
    where
        F: Fn(&ParsedCommandLine) -> Result<Vec<CompletionCandidate>, GeneratorError>,
    {
        let corrector = ContextCorrector::new(self.database);
        if let Some((cmd_index, cmd_name)) = corrector.find_command_with_args_arg(parsed) {
            let inner_parsed = corrector.reparse_inner_command(parsed, cmd_index, cmd_name);
            let mut candidates =
                if let Some(command_completion) = self.database.get_command(&parsed.command) {
                    OptionGenerator::generate_short_option_candidates(command_completion, parsed)
                        .map_err(GeneratorError::Other)?
                } else {
                    Vec::new()
                };

            candidates.extend(recurse_cb(&inner_parsed)?);
            return Ok(candidates);
        }

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            OptionGenerator::generate_short_option_candidates(command_completion, parsed)
                .map_err(GeneratorError::Other)
        } else {
            Err(GeneratorError::MissingCommand(parsed.command.clone()))
        }
    }

    /// Generate long option completion candidates
    pub fn generate_long_option_candidates<F>(
        &self,
        parsed: &ParsedCommandLine,
        recurse_cb: F,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError>
    where
        F: Fn(&ParsedCommandLine) -> Result<Vec<CompletionCandidate>, GeneratorError>,
    {
        let corrector = ContextCorrector::new(self.database);
        if let Some((cmd_index, cmd_name)) = corrector.find_command_with_args_arg(parsed) {
            let inner_parsed = corrector.reparse_inner_command(parsed, cmd_index, cmd_name);
            let mut candidates =
                if let Some(command_completion) = self.database.get_command(&parsed.command) {
                    OptionGenerator::generate_long_option_candidates(command_completion, parsed)
                        .map_err(GeneratorError::Other)?
                } else {
                    Vec::new()
                };

            candidates.extend(recurse_cb(&inner_parsed)?);
            return Ok(candidates);
        }

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            OptionGenerator::generate_long_option_candidates(command_completion, parsed)
                .map_err(GeneratorError::Other)
        } else {
            Err(GeneratorError::MissingCommand(parsed.command.clone()))
        }
    }

    /// Generate completion candidates based on type
    pub fn generate_candidates_for_type(
        &self,
        arg_type: &ArgumentType,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        match arg_type {
            ArgumentType::File { extensions } => {
                FileSystemGenerator::generate_file_candidates_with_filter(
                    &parsed.current_token,
                    extensions.as_ref(),
                )
            }
            ArgumentType::Directory => {
                FileSystemGenerator::generate_directory_candidates(&parsed.current_token)
            }
            ArgumentType::Choice(choices) => Ok(choices
                .iter()
                .filter(|choice| fuzzy_match_score(choice, &parsed.current_token).is_some())
                .map(|choice| CompletionCandidate::argument(choice.clone(), None))
                .collect()),
            ArgumentType::Command => {
                SystemCommandGenerator::new().generate_candidates(&parsed.current_token)
            }
            ArgumentType::Environment => {
                let mut candidates = Vec::with_capacity(32);
                for (key, _) in std::env::vars() {
                    if fuzzy_match_score(&key, &parsed.current_token).is_some() {
                        candidates.push(CompletionCandidate::argument(key, None));
                    }
                }
                Ok(candidates)
            }
            ArgumentType::Script(command) => {
                ScriptGenerator::default().generate_script_candidates(command, parsed)
            }
            ArgumentType::Process => {
                ProcessGenerator::new().generate_candidates(&parsed.current_token)
            }
            ArgumentType::CommandWithArgs => {
                SystemCommandGenerator::new().generate_candidates(&parsed.current_token)
            }
            ArgumentType::User => UserGenerator::new().generate_candidates(&parsed.current_token),
            ArgumentType::Group => GroupGenerator::new().generate_candidates(&parsed.current_token),
            ArgumentType::Signal => {
                SignalGenerator::new().generate_candidates(&parsed.current_token)
            }
            ArgumentType::Interface => {
                InterfaceGenerator::new().generate_candidates(&parsed.current_token)
            }
            _ => Ok(Vec::new()),
        }
    }
}
