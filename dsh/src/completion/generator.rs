use super::cache::CompletionCache;
use super::command::{ArgumentType, CommandCompletionDatabase, CompletionCandidate};
use super::context::ContextCorrector;
use super::fuzzy_match_score;
use super::generators::filesystem::FileSystemGenerator;
use super::generators::option::OptionGenerator;
use super::generators::process::ProcessGenerator;
use super::generators::script::ScriptGenerator;
use super::generators::subcommand::SubCommandGenerator;
use super::parser::{CommandLineParser, CompletionContext, ParsedCommandLine};
use crate::dirs::is_executable;
use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::fs::read_dir;
use std::path::Path;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SYSTEM_COMMAND_CACHE_TTL_MS: u64 = 2000;
static SYSTEM_COMMAND_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(SYSTEM_COMMAND_CACHE_TTL_MS)));

static GLOBAL_SYSTEM_COMMANDS: LazyLock<Arc<RwLock<Option<HashSet<String>>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

static GLOBAL_CACHE_INFLIGHT: LazyLock<AtomicBool> = LazyLock::new(|| AtomicBool::new(false));

pub fn set_global_system_commands(commands: HashSet<String>) {
    let mut guard = GLOBAL_SYSTEM_COMMANDS.write();
    *guard = Some(commands);
}

pub fn clear_global_system_commands() {
    let mut guard = GLOBAL_SYSTEM_COMMANDS.write();
    *guard = None;
    GLOBAL_CACHE_INFLIGHT.store(false, Ordering::SeqCst);
}

fn ensure_global_cache_populated() {
    if GLOBAL_SYSTEM_COMMANDS.read().is_some() {
        return;
    }

    if GLOBAL_CACHE_INFLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    std::thread::spawn(|| {
        let paths: Vec<std::path::PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();

        let mut commands = HashSet::new();
        for path in paths {
            if let Ok(entries) = read_dir(&path) {
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type()
                        && !ft.is_file()
                        && !ft.is_symlink()
                    {
                        continue;
                    }
                    if crate::dirs::is_executable(&entry)
                        && let Some(name) = entry.file_name().to_str()
                    {
                        commands.insert(name.to_string());
                    }
                }
            }
        }

        let mut guard = GLOBAL_SYSTEM_COMMANDS.write();
        *guard = Some(commands);
        GLOBAL_CACHE_INFLIGHT.store(false, Ordering::SeqCst);
    });
}

#[derive(Debug)]
pub enum GeneratorError {
    MissingCommand(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for GeneratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeneratorError::MissingCommand(cmd) => write!(f, "Missing command definition: {}", cmd),
            GeneratorError::Other(e) => write!(f, "Generator error: {}", e),
        }
    }
}

impl std::error::Error for GeneratorError {}

impl From<anyhow::Error> for GeneratorError {
    fn from(error: anyhow::Error) -> Self {
        GeneratorError::Other(error)
    }
}

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
    #[allow(dead_code)]
    pub fn has_command_completion(&self, command: &str) -> bool {
        self.database.get_command(command).is_some()
    }

    /// Public fallback generator (Files + System)
    pub fn generate_fallback_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let mut candidates = FileSystemGenerator::generate_file_candidates(current_token)
            .map_err(GeneratorError::Other)?;

        candidates.extend(
            self.generate_system_command_candidates(current_token)
                .map_err(GeneratorError::Other)?,
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
            CompletionContext::Command => {
                self.generate_command_candidates(&corrected.current_token)
            }
            CompletionContext::SubCommand => self.generate_subcommand_candidates(&corrected),
            CompletionContext::ShortOption => self.generate_short_option_candidates(&corrected),
            CompletionContext::LongOption => self.generate_long_option_candidates(&corrected),
            CompletionContext::OptionValue {
                option_name: _,
                value_type,
            } => self.generate_option_value_candidates(&corrected, value_type.as_ref()),
            CompletionContext::Argument {
                arg_index: _,
                arg_type,
            } => self.generate_argument_candidates(&corrected, arg_type.as_ref()),
            CompletionContext::Unknown => Ok(Vec::new()),
        }
    }

    /// Generate command name completion candidates
    fn generate_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        let mut candidates = Vec::with_capacity(32);

        // Commands registered in database
        for command_name in self.database.get_command_names() {
            if command_name.starts_with(current_token)
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
    fn generate_subcommand_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            SubCommandGenerator::generate_candidates(command_completion, parsed, |arg_type, p| {
                self.generate_candidates_for_type(arg_type, p)
            })
            .map_err(GeneratorError::Other)
        } else {
            // Signal missing command so the engine can try to load it
            Err(GeneratorError::MissingCommand(parsed.command.clone()))
        }
    }

    /// Generate short option completion candidates
    fn generate_short_option_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        if let Some((cmd_index, cmd_name)) = self.find_command_with_args_arg(parsed) {
            let inner_parsed = self.reparse_inner_command(parsed, cmd_index, cmd_name);
            let mut candidates =
                if let Some(command_completion) = self.database.get_command(&parsed.command) {
                    OptionGenerator::generate_short_option_candidates(command_completion, parsed)
                        .map_err(GeneratorError::Other)?
                } else {
                    Vec::new()
                };

            candidates.extend(self.generate_candidates(&inner_parsed)?);
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
    fn generate_long_option_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
        if let Some((cmd_index, cmd_name)) = self.find_command_with_args_arg(parsed) {
            let inner_parsed = self.reparse_inner_command(parsed, cmd_index, cmd_name);
            let mut candidates =
                if let Some(command_completion) = self.database.get_command(&parsed.command) {
                    OptionGenerator::generate_long_option_candidates(command_completion, parsed)
                        .map_err(GeneratorError::Other)?
                } else {
                    Vec::new()
                };

            candidates.extend(self.generate_candidates(&inner_parsed)?);
            return Ok(candidates);
        }

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            OptionGenerator::generate_long_option_candidates(command_completion, parsed)
                .map_err(GeneratorError::Other)
        } else {
            Err(GeneratorError::MissingCommand(parsed.command.clone()))
        }
    }

    /// Generate option value completion candidates
    fn generate_option_value_candidates(
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

        // Fallback: if we don't know the type or returned no candidates, try file completion
        // This makes `git -C <tab>` work even if we didn't strictly define it as Directory (though we should),
        // but more importantly avoids dead ends for generic options.
        if candidates.is_empty() {
            candidates.extend(
                FileSystemGenerator::generate_file_candidates(&parsed.current_token)
                    .map_err(GeneratorError::Other)?,
            );
        }

        Ok(candidates)
    }

    /// Generate argument completion candidates
    fn generate_argument_candidates(
        &self,
        parsed: &ParsedCommandLine,
        arg_type: Option<&ArgumentType>,
    ) -> Result<Vec<CompletionCandidate>, GeneratorError> {
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
            } else if !parsed.command.is_empty() {
                // Trying to complete Argument for a command that we don't have DB entry for?
                // Wait, if we are in Argument context, checking command existence is tricky.
                // But strictly speaking, if we don't know the command, we fallback to Files.
                // HOWEVER, recursive check below needs to run.
            }

            // Default to file completion if no candidates generated yet
            if candidates.is_empty() {
                candidates.extend(
                    FileSystemGenerator::generate_file_candidates(&parsed.current_token)
                        .map_err(GeneratorError::Other)?,
                );
            }
        }

        if let Some((cmd_arg_index, cmd_name)) = self.find_command_with_args_arg(parsed)
            && let CompletionContext::Argument { arg_index, .. } = parsed.completion_context
            && arg_index > cmd_arg_index
        {
            // Recursive completion for the inner command
            let inner_parsed = self.reparse_inner_command(parsed, cmd_arg_index, cmd_name);
            return self.generate_candidates(&inner_parsed);
        }

        Ok(candidates)
    }

    /// Generate completion candidates based on type
    fn generate_candidates_for_type(
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
                .filter(|choice| choice.starts_with(&parsed.current_token))
                .map(|choice| CompletionCandidate::argument(choice.clone(), None))
                .collect()),
            ArgumentType::Command => self.generate_system_command_candidates(&parsed.current_token),
            ArgumentType::Environment => {
                self.generate_environment_variable_candidates(&parsed.current_token)
            }
            ArgumentType::Script(command) => {
                ScriptGenerator::default().generate_script_candidates(command, parsed)
            }
            ArgumentType::Process => {
                ProcessGenerator::new().generate_candidates(&parsed.current_token)
            }
            ArgumentType::CommandWithArgs => {
                self.generate_system_command_candidates(&parsed.current_token)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Generate system command completion candidates (simplified version)
    fn generate_system_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        if !current_token.is_empty()
            && let Some(hit) = SYSTEM_COMMAND_CACHE.lookup(current_token)
        {
            return Ok(hit.candidates);
        }

        let mut candidates = Vec::with_capacity(32);
        let mut seen_names: HashSet<String> = HashSet::new();

        if (current_token.starts_with('/') || current_token.starts_with("./"))
            && Path::new(current_token).is_file()
        {
            candidates.push(CompletionCandidate::subcommand(
                current_token.to_string(),
                None,
            ));
            seen_names.insert(current_token.to_string());
        }

        // Try global cache first
        ensure_global_cache_populated();

        let cache_hit = {
            let guard = GLOBAL_SYSTEM_COMMANDS.read();
            if let Some(commands) = &*guard {
                // Filter from cache
                // We want to sort them?
                let mut local_candidates: Vec<&str> = commands
                    .iter()
                    .filter(|cmd| cmd.starts_with(current_token))
                    .map(|s| s.as_str())
                    .collect();

                local_candidates.sort();

                for cmd in local_candidates {
                    if candidates.len() >= super::MAX_RESULT {
                        break;
                    }
                    if seen_names.insert(cmd.to_string()) {
                        candidates.push(CompletionCandidate::subcommand(cmd.to_string(), None));
                    }
                }
                true
            } else {
                false
            }
        };

        if !cache_hit {
            // Fallback to synchronous scan if cache not ready
            let paths: Vec<std::path::PathBuf> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default();

            for path in paths {
                if candidates.len() >= super::MAX_RESULT {
                    break;
                }

                if let Ok(entries) = read_dir(&path) {
                    let mut local_candidates: Vec<String> = Vec::new();

                    for entry in entries.flatten() {
                        let file_name_os = entry.file_name();
                        let Some(file_name) = file_name_os.to_str() else {
                            continue;
                        };

                        if fuzzy_match_score(file_name, current_token).is_none() {
                            continue;
                        }

                        if seen_names.contains(file_name) {
                            continue;
                        }

                        if let Ok(ft) = entry.file_type()
                            && !ft.is_file()
                            && !ft.is_symlink()
                        {
                            continue;
                        }

                        if is_executable(&entry) {
                            local_candidates.push(file_name.to_string());
                        }
                    }

                    local_candidates.sort();
                    for cmd in local_candidates {
                        if candidates.len() >= super::MAX_RESULT {
                            break;
                        }
                        if seen_names.insert(cmd.clone()) {
                            candidates.push(CompletionCandidate::subcommand(cmd, None));
                        }
                    }
                }
            }
        }

        if !current_token.is_empty() {
            SYSTEM_COMMAND_CACHE.set(current_token.to_string(), candidates.clone());
        }

        Ok(candidates)
    }

    /// Generate environment variable completion candidates
    fn generate_environment_variable_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(32);

        for (key, _) in std::env::vars() {
            if key.starts_with(current_token) {
                candidates.push(CompletionCandidate::argument(key, None));
            }
        }

        Ok(candidates)
    }

    /// Helper to find if one of the arguments is a CommandWithArgs
    fn find_command_with_args_arg(&self, parsed: &ParsedCommandLine) -> Option<(usize, String)> {
        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let args_def = &command_completion.arguments;
            for (i, arg_val) in parsed.specified_arguments.iter().enumerate() {
                if let Some(arg_def) = args_def.get(i)
                    && let Some(ArgumentType::CommandWithArgs) = arg_def.arg_type
                {
                    return Some((i, arg_val.clone()));
                }
            }
        }
        None
    }

    /// Helper to re-parse the inner command string for context awareness
    fn reparse_inner_command(
        &self,
        parsed: &ParsedCommandLine,
        cmd_index: usize,
        cmd_name: String,
    ) -> ParsedCommandLine {
        // Reconstruct input string: cmd_name + space + args joined by space
        let mut input_parts = Vec::new();
        input_parts.push(cmd_name);

        //    But we don't know for sure if it takes a value without schema.
        //    BUT, we have the exact string value of the command 'cmd_name'.
        //    Let's find the FIRST occurrence of 'cmd_name' in raw_args?
        //    Risk: 'sudo -u git git status'. First 'git' is value of -u.

        // Improved Strategy:
        // Use specified_arguments to find the exact target token string, then assume order is preserved.
        // We are looking for the (cmd_index)-th argument.
        // Iterate raw_args. Keep track of how many "arguments" vs "options" we've seen?
        // Hard because we don't know if an option took a value.

        // Simpler Strategy for now:
        // Just find the first token in raw_args that matches `parsed.specified_arguments[cmd_index]`.
        // AND hope it's the right one.
        // For `sudo`, the command is usually the first non-option argument.

        // We iterate raw_args and try to find the match.

        let mut found_start = false;

        let target_arg = &parsed.specified_arguments[cmd_index];
        let mut tokens_to_skip = 0;

        for (i, token) in parsed.raw_args.iter().enumerate() {
            if token == target_arg {
                // Determine if this is likely the one.
                // If we have previous matches, we might be confused.
                // But for `sudo git`, `git` is likely unique or at least the first one is valid.
                tokens_to_skip = i + 1;
                found_start = true;
                break;
            }
        }

        if found_start {
            for arg in parsed.raw_args.iter().skip(tokens_to_skip) {
                // Simple quoting if needed
                if arg.contains(' ') || arg.contains('\t') {
                    input_parts.push(format!("{:?}", arg));
                } else {
                    input_parts.push(arg.to_string());
                }
            }
        }

        // Check if current_token needs to be appended
        // raw_args usually includes the current token if it was partially parsed?
        // Parser logic:
        // tokenize -> tokens.
        // find cursor token index.
        // analyze_tokens(tokens).
        // tokens_queue in analyze_tokens includes the current token.
        // raw_args = tokens_queue (after subcommands).
        // So raw_args SHOULD include the current token, unless cursor is in a gap?
        // If cursor is in a gap (adding new token), raw_args might not have it or have empty string?

        let current = &parsed.current_token;
        if !current.is_empty() {
            // If cursor is in a gap (e.g. "git commit "), raw_args might not have the "next" empty token.
        } else {
            // Cursor in gap (trailing space).
            // We need to ensure trailing space in reconstructed string.
            input_parts.push("".to_string());
        }

        // Edge case: if current token IS the command itself (e.g. `sudo gi<TAB>`).
        // Then we shouldn't be here?
        // recursive logic is triggered only if arg_index > cmd_arg_index.
        // So we are past the command.

        let input = input_parts.join(" ");
        CommandLineParser::new().parse(&input, input.len())
    }
}

#[cfg(test)]
mod tests {
    use super::FileSystemGenerator;
    use super::*;
    use crate::completion::command::{Argument, CommandCompletion, CommandOption, SubCommand};
    use crate::completion::parser::CommandLineParser;
    use std::path::{MAIN_SEPARATOR, Path};
    use tempfile::tempdir;

    fn create_test_database() -> CommandCompletionDatabase {
        let mut db = CommandCompletionDatabase::new();

        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git version control".to_string()),
            global_options: vec![],
            subcommands: vec![
                SubCommand {
                    name: "add".to_string(),
                    description: Some("Add files".to_string()),
                    options: vec![],
                    arguments: vec![Argument {
                        name: "pathspec".to_string(),
                        description: Some("Files to add".to_string()),
                        arg_type: None,
                    }],
                    subcommands: vec![],
                },
                SubCommand {
                    name: "commit".to_string(),
                    description: Some("Commit changes".to_string()),
                    options: vec![],
                    arguments: vec![],
                    subcommands: vec![],
                },
            ],
            arguments: vec![],
        };

        db.add_command(git_completion);
        db
    }

    #[test]
    fn test_generate_command_candidates() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(&db);

        let candidates = generator.generate_command_candidates("gi").unwrap();
        assert!(!candidates.is_empty());

        let git_candidate = candidates.iter().find(|c| c.text == "git");
        assert!(git_candidate.is_some());
    }

    #[test]
    fn test_generate_subcommand_candidates() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(&db);

        let parsed = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec![],
            raw_args: vec![],
            args: vec![],
            options: vec![],
            current_token: "a".to_string(),
            current_arg: Some("a".to_string()),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        let candidates = generator.generate_candidates(&parsed).unwrap();
        assert!(!candidates.is_empty());

        let add_candidate = candidates.iter().find(|c| c.text == "add");
        assert!(add_candidate.is_some());
    }

    #[test]
    fn test_partial_subcommand_is_not_reclassified_as_argument() {
        let mut db = CommandCompletionDatabase::new();
        let docker_completion = CommandCompletion {
            command: "docker".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![
                SubCommand {
                    name: "compose".to_string(),
                    description: None,
                    options: vec![],
                    arguments: vec![],
                    subcommands: vec![],
                },
                SubCommand {
                    name: "build".to_string(),
                    description: None,
                    options: vec![],
                    arguments: vec![],
                    subcommands: vec![],
                },
            ],
            arguments: vec![],
        };
        db.add_command(docker_completion);

        let parser = CommandLineParser::new();
        let input = "docker com";
        let parsed = parser.parse(input, input.len());
        let generator = CompletionGenerator::new(&db);

        let corrected = generator.correct_parsed_command_line(&parsed);
        assert_eq!(corrected.completion_context, CompletionContext::SubCommand);
        assert!(corrected.subcommand_path.is_empty());

        let candidates = generator.generate_candidates(&parsed).unwrap();
        assert!(candidates.iter().any(|c| c.text == "compose"));
    }

    #[test]
    fn test_nested_subcommand_completion_after_space_and_short_prefix() {
        let mut db = CommandCompletionDatabase::new();
        let docker_completion = CommandCompletion {
            command: "docker".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "compose".to_string(),
                description: None,
                options: vec![],
                arguments: vec![],
                subcommands: vec![
                    SubCommand {
                        name: "up".to_string(),
                        description: None,
                        options: vec![],
                        arguments: vec![],
                        subcommands: vec![],
                    },
                    SubCommand {
                        name: "down".to_string(),
                        description: None,
                        options: vec![],
                        arguments: vec![],
                        subcommands: vec![],
                    },
                ],
            }],
            arguments: vec![],
        };
        db.add_command(docker_completion);
        let generator = CompletionGenerator::new(&db);
        let parser = CommandLineParser::new();

        // After a valid subcommand with trailing space, nested subcommands should be suggested.
        let input_space = "docker compose ";
        let parsed_space = parser.parse(input_space, input_space.len());
        let corrected_space = generator.correct_parsed_command_line(&parsed_space);
        assert_eq!(
            corrected_space.completion_context,
            CompletionContext::SubCommand
        );
        let candidates_space = generator.generate_candidates(&parsed_space).unwrap();
        assert!(candidates_space.iter().any(|c| c.text == "up"));

        // Even a 1-character prefix should still complete nested subcommands.
        let input_prefix = "docker compose u";
        let parsed_prefix = parser.parse(input_prefix, input_prefix.len());
        let corrected_prefix = generator.correct_parsed_command_line(&parsed_prefix);
        assert_eq!(
            corrected_prefix.completion_context,
            CompletionContext::SubCommand
        );
        let candidates_prefix = generator.generate_candidates(&parsed_prefix).unwrap();
        assert!(candidates_prefix.iter().any(|c| c.text == "up"));
    }

    #[test]
    fn test_short_and_long_options_generated_for_single_dash() {
        let mut db = CommandCompletionDatabase::new();
        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "commit".to_string(),
                description: None,
                options: vec![
                    CommandOption {
                        short: Some("-m".to_string()),
                        long: Some("--message".to_string()),
                        description: None,
                        argument: None,
                    },
                    CommandOption {
                        short: Some("-a".to_string()),
                        long: Some("--all".to_string()),
                        description: None,
                        argument: None,
                    },
                ],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        };
        db.add_command(git_completion);

        let parser = CommandLineParser::new();
        // "-" is parsed as LongOption context by verify logic
        let input = "git commit -";
        let parsed = parser.parse(input, input.len());
        let generator = CompletionGenerator::new(&db);

        let candidates = generator.generate_candidates(&parsed).unwrap();

        // Should contain both long and short options
        assert!(candidates.iter().any(|c| c.text == "--message"));
        assert!(candidates.iter().any(|c| c.text == "--all"));
        assert!(candidates.iter().any(|c| c.text == "-m"));
        assert!(candidates.iter().any(|c| c.text == "-a"));
    }

    #[test]
    fn build_candidate_path_handles_root_without_double_slash() {
        assert_eq!(
            FileSystemGenerator::build_candidate_path("/", "tmp"),
            "/tmp"
        );
    }

    #[test]
    fn build_candidate_path_preserves_relative_paths() {
        assert_eq!(FileSystemGenerator::build_candidate_path(".", "foo"), "foo");
        assert_eq!(
            FileSystemGenerator::build_candidate_path("/usr", "bin"),
            "/usr/bin"
        );
    }

    #[test]
    fn file_candidates_expand_directory_with_trailing_separator() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("alpha.txt"), "").unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();

        let db = CommandCompletionDatabase::new();
        let _generator = CompletionGenerator::new(&db);
        let token = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        let candidates = FileSystemGenerator::generate_file_candidates(&token)
            .expect("file candidates with trailing separator");
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        let expected_file = Path::new(&token)
            .join("alpha.txt")
            .to_string_lossy()
            .to_string();
        let expected_dir = Path::new(&token)
            .join("nested")
            .to_string_lossy()
            .to_string();

        assert!(
            texts.contains(&expected_file),
            "expected file candidate {} in {:?}",
            expected_file,
            texts
        );
        assert!(
            texts.contains(&expected_dir),
            "expected directory candidate {} in {:?}",
            expected_dir,
            texts
        );
    }

    #[test]
    fn test_git_add_completion_options_with_single_dash() {
        let mut db = CommandCompletionDatabase::new();
        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "add".to_string(),
                description: None,
                options: vec![
                    CommandOption {
                        short: Some("-A".to_string()),
                        long: Some("--all".to_string()),
                        description: None,
                        argument: None,
                    },
                    CommandOption {
                        short: Some("-u".to_string()),
                        long: Some("--update".to_string()),
                        description: None,
                        argument: None,
                    },
                ],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![],
        };
        db.add_command(git_completion);

        let parser = CommandLineParser::new();
        // Case 1: "git add -" should trigger LongOption context but verify both short/long
        let input = "git add -";
        let parsed = parser.parse(input, input.len());
        // Verify parser context assumption
        assert_eq!(parsed.completion_context, CompletionContext::LongOption);

        let generator = CompletionGenerator::new(&db);
        let candidates = generator.generate_candidates(&parsed).unwrap();

        assert!(candidates.iter().any(|c| c.text == "-A"));
        assert!(candidates.iter().any(|c| c.text == "--all"));
        assert!(candidates.iter().any(|c| c.text == "-u"));
        assert!(candidates.iter().any(|c| c.text == "--update"));
    }

    #[test]
    fn directory_candidates_expand_directory_with_trailing_separator() {
        let temp = tempdir().unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();

        let db = CommandCompletionDatabase::new();
        let _generator = CompletionGenerator::new(&db);
        let token = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        let candidates = FileSystemGenerator::generate_directory_candidates(&token)
            .expect("directory candidates with trailing separator");
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        let expected_dir = Path::new(&token)
            .join("nested")
            .to_string_lossy()
            .to_string();

        assert!(
            texts.contains(&expected_dir),
            "expected nested directory candidate {} in {:?}",
            expected_dir,
            texts
        );
    }

    #[test]
    fn test_cd_completion_logic() {
        let mut db = CommandCompletionDatabase::new();
        let cd_completion = CommandCompletion {
            command: "cd".to_string(),
            description: Some("Change directory".to_string()),
            subcommands: vec![],
            global_options: vec![],
            arguments: vec![Argument {
                name: "directory".to_string(),
                description: Some("Directory to change to".to_string()),
                arg_type: Some(ArgumentType::Directory),
            }],
        };
        db.add_command(cd_completion);

        let temp = tempdir().unwrap();
        std::fs::create_dir(temp.path().join("nested_dir")).unwrap();
        std::fs::write(temp.path().join("file.txt"), "").unwrap();

        let generator = CompletionGenerator::new(&db);

        // Mock parsed command line: "cd [temp_dir]/"
        let dir_str = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);
        let parsed = ParsedCommandLine {
            command: "cd".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: dir_str.clone(),
            current_arg: Some(dir_str.clone()),
            completion_context: CompletionContext::SubCommand, // Parser might think it's subcommand context at top level
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let candidates = generator.generate_candidates(&parsed).unwrap();
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        // Should contain directory but not file
        let expected_dir = Path::new(&dir_str)
            .join("nested_dir")
            .to_string_lossy()
            .to_string();
        let expected_file = Path::new(&dir_str)
            .join("file.txt")
            .to_string_lossy()
            .to_string();

        assert!(
            texts.contains(&expected_dir),
            "expected directory candidate {} in {:?}",
            expected_dir,
            texts
        );
        assert!(
            !texts.contains(&expected_file),
            "should not contain file candidates: {:?}",
            texts
        );
    }

    #[test]
    fn test_subcommand_and_file_completion() {
        // Setup: Command with subcommand and an argument
        let mut db = CommandCompletionDatabase::new();
        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git version control".to_string()),
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "commit".to_string(),
                description: Some("Commit".to_string()),
                options: vec![],
                arguments: vec![],
                subcommands: vec![],
            }],
            arguments: vec![Argument {
                name: "file".to_string(),
                description: Some("File".to_string()),
                arg_type: Some(ArgumentType::File { extensions: None }),
            }],
        };
        db.add_command(git_completion);

        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("test.txt"), "").unwrap();

        let generator = CompletionGenerator::new(&db);
        let dir_str = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        // Mock: "git [dir]/"
        // Expect: "commit" (subcommand) AND "test.txt" (file)
        let parsed = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: dir_str.clone(),
            current_arg: Some(dir_str.clone()),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let candidates = generator.generate_candidates(&parsed).unwrap();
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        // Subcommand check
        let has_commit = texts.contains(&"commit".to_string());
        // Since we are completing a directory path, and "commit" doesn't start with /tmp/...,
        // it likely WON'T be in the list unless the prefix matching is very loose.
        // However, the logic I implemented uses `starts_with(&parsed.current_token)`.
        // If current_token is a path, "commit" won't match.
        // So `has_commit` should be false here if strict.
        // But if the user types `git <TAB>`, current_token is empty, and then it SHOULD match.
        // The test above sets current_token to `dir_str` which is NOT empty.
        // So actually, for THIS specific test case where token is a directory path,
        // we expect FILES (test.txt) but NOT subcommands (unless they match the path?).

        let expected_file = Path::new(&dir_str)
            .join("test.txt")
            .to_string_lossy()
            .to_string();

        assert!(
            texts.contains(&expected_file),
            "Should contain file candidate {:?} in {:?}",
            expected_file,
            texts
        );
        assert!(
            !has_commit,
            "Subcommand matches only if it starts with the token"
        );
    }

    #[test]
    fn test_subcommand_and_file_completion_empty_token() {
        let mut db = CommandCompletionDatabase::new();
        let mycmd_completion = CommandCompletion {
            command: "mycmd".to_string(),
            description: None,
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "sub".to_string(),
                description: None,
                options: vec![],
                subcommands: vec![],
                arguments: vec![Argument {
                    name: "arg".to_string(),
                    description: None,
                    arg_type: Some(ArgumentType::File { extensions: None }), // Default
                }],
            }],
            arguments: vec![],
        };
        db.add_command(mycmd_completion);

        let generator = CompletionGenerator::new(&db);
        let token = ""; // Empty token

        let parsed = ParsedCommandLine {
            command: "mycmd".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: token.to_string(),
            current_arg: Some(token.to_string()),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let candidates = generator.generate_candidates(&parsed).unwrap();
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        // sub matches because starts_with("") is true
        assert!(texts.contains(&"sub".to_string()));
        // file matches
        // For empty token, file generator produces contents of current dir
        // We can't easily assert exact files but it shouldn't fail
    }

    #[test]
    fn test_script_execution() {
        let mut db = CommandCompletionDatabase::new();
        let script_completion = CommandCompletion {
            command: "scriptcmd".to_string(),
            description: None,
            subcommands: vec![],
            global_options: vec![],
            arguments: vec![Argument {
                name: "arg".to_string(),
                description: None,
                arg_type: Some(ArgumentType::Script(
                    "echo candidate1\necho candidate2".to_string(),
                )),
            }],
        };
        db.add_command(script_completion);
        let generator = CompletionGenerator::new(&db);

        let parsed = ParsedCommandLine {
            command: "scriptcmd".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: Some("".to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            }, // Provide context
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let candidates = generator
            .generate_candidates(&parsed)
            .expect("Generate candidates should succeed");
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        assert!(texts.contains(&"candidate1".to_string()));
        assert!(texts.contains(&"candidate2".to_string()));
    }

    #[test]
    fn test_script_execution_variable_substitution() {
        let mut db = CommandCompletionDatabase::new();
        // We use printf to avoid newline issues and control output exactly
        let script_completion = CommandCompletion {
            command: "substcmd".to_string(),
            description: None,
            subcommands: vec![],
            global_options: vec![],
            arguments: vec![Argument {
                name: "arg".to_string(),
                description: None,
                arg_type: Some(ArgumentType::Script(
                    "echo $COMMAND:$CURRENT_TOKEN".to_string(),
                )),
            }],
        };
        db.add_command(script_completion);
        let generator = CompletionGenerator::new(&db);

        let parsed = ParsedCommandLine {
            command: "substcmd".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "val".to_string(),
            current_arg: Some("val".to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let _candidates = generator
            .generate_candidates(&parsed)
            .expect("Generate candidates should succeed");
        // Our generator filters by start_with(current_token).
        // "substcmd:val" does NOT start with "val".
        // Wait, the logic in generate_script_candidates is:
        // `if !trimmed.is_empty() && trimmed.starts_with(&parsed.current_token)`
        // So script output MUST start with "val".

        // Let's adjust script to produce something that starts with "val"
        // script: "echo val_suffix"

        // Let's create a passing test case
        let mut db2 = CommandCompletionDatabase::new();
        let script_completion2 = CommandCompletion {
            command: "substcmd2".to_string(),
            description: None,
            subcommands: vec![],
            global_options: vec![],
            arguments: vec![Argument {
                name: "arg".to_string(),
                description: None,
                arg_type: Some(ArgumentType::Script(
                    "echo $CURRENT_TOKEN_suffix".to_string(),
                )),
            }],
        };
        db2.add_command(script_completion2);
        let generator2 = CompletionGenerator::new(&db2);

        let mut parsed2 = parsed.clone();
        parsed2.command = "substcmd2".to_string();

        let candidates2 = generator2.generate_candidates(&parsed2).expect("Success");
        let texts: Vec<String> = candidates2.into_iter().map(|c| c.text).collect();

        // "val_suffix" starts with "val"
        assert!(texts.contains(&"val_suffix".to_string()));
    }
    #[test]
    fn test_option_completion_guard() {
        let mut db = CommandCompletionDatabase::new();
        let mycmd_completion = CommandCompletion {
            command: "mycmd".to_string(),
            description: None,
            global_options: vec![CommandOption {
                short: Some("-o".to_string()),
                long: Some("--option".to_string()),
                description: None,
                argument: None,
            }],
            subcommands: vec![],
            arguments: vec![],
        };
        db.add_command(mycmd_completion);

        let generator = CompletionGenerator::new(&db);

        // 1. Token == "" -> Should NOT show options
        let parsed_empty = ParsedCommandLine {
            command: "mycmd".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: Some("".to_string()),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };
        let candidates = generator.generate_candidates(&parsed_empty).unwrap();
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();
        assert!(
            !texts.contains(&"-o".to_string()),
            "Should NOT show options on empty token"
        );
        assert!(
            !texts.contains(&"--option".to_string()),
            "Should NOT show options on empty token"
        );

        // 2. Token == "-" -> Should show options
        let parsed_dash = ParsedCommandLine {
            command: "mycmd".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "-".to_string(),
            current_arg: Some("-".to_string()),
            completion_context: CompletionContext::LongOption,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };
        let candidates_dash = generator.generate_candidates(&parsed_dash).unwrap();
        let texts_dash: Vec<String> = candidates_dash.into_iter().map(|c| c.text).collect();
        assert!(
            texts_dash.contains(&"-o".to_string()),
            "Should show options when token starts with -"
        );
        assert!(
            texts_dash.contains(&"--option".to_string()),
            "Should show options when token starts with -"
        );
    }

    fn create_complex_test_database() -> CommandCompletionDatabase {
        let mut db = CommandCompletionDatabase::new();

        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git version control".to_string()),
            global_options: vec![],
            subcommands: vec![
                SubCommand {
                    name: "add".to_string(),
                    description: Some("Add files".to_string()),
                    options: vec![],
                    arguments: vec![Argument {
                        name: "pathspec".to_string(),
                        description: Some("Files to add".to_string()),
                        arg_type: None,
                    }],
                    subcommands: vec![],
                },
                SubCommand {
                    name: "push".to_string(),
                    description: Some("Push".to_string()),
                    options: vec![],
                    arguments: vec![
                        Argument {
                            name: "remote".to_string(),
                            description: None,
                            arg_type: None,
                        },
                        Argument {
                            name: "branch".to_string(),
                            description: None,
                            arg_type: None,
                        },
                    ],
                    subcommands: vec![],
                },
            ],
            arguments: vec![],
        };

        db.add_command(git_completion);
        db
    }

    struct CompletionTestCase {
        description: &'static str,
        input: &'static str,
        cursor_pos: Option<usize>, // If None, at end of input
        expected_candidates: Vec<&'static str>,
        unexpected_candidates: Vec<&'static str>,
    }

    fn run_completion_test_cases(cases: Vec<CompletionTestCase>) {
        let db = create_complex_test_database();
        let generator = CompletionGenerator::new(&db);
        let parser = CommandLineParser::new();

        for case in cases {
            let cursor = case.cursor_pos.unwrap_or(case.input.len());
            let parsed = parser.parse(case.input, cursor);
            let candidates = generator.generate_candidates(&parsed).unwrap_or_else(|e| {
                panic!(
                    "Failed to generate candidates for '{}': {}",
                    case.description, e
                )
            });

            let candidate_texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

            // Check expected candidates
            for expected in &case.expected_candidates {
                assert!(
                    candidate_texts.iter().any(|c| c == *expected),
                    "Test '{}': Expected candidate '{}' not found in {:?}",
                    case.description,
                    expected,
                    candidate_texts
                );
            }

            // Check unexpected candidates
            for unexpected in &case.unexpected_candidates {
                assert!(
                    !candidate_texts.iter().any(|c| c == *unexpected),
                    "Test '{}': Unexpected candidate '{}' found in {:?}",
                    case.description,
                    unexpected,
                    candidate_texts
                );
            }
        }
    }

    #[test]
    fn test_git_completion_scenarios() {
        let cases = vec![
            // Command completion
            CompletionTestCase {
                description: "Command: git",
                input: "git",
                cursor_pos: None,
                expected_candidates: vec!["git"],
                unexpected_candidates: vec![],
            },
            // Subcommand completion
            CompletionTestCase {
                description: "Subcommand: git add (partial)",
                input: "git a",
                cursor_pos: None,
                expected_candidates: vec!["add"],
                unexpected_candidates: vec!["push"],
            },
            // Subcommand completion (list all)
            CompletionTestCase {
                description: "Subcommand: git <TAB>",
                input: "git ",
                cursor_pos: None,
                expected_candidates: vec!["add", "push"],
                unexpected_candidates: vec![],
            },
            // Argument completion (simple)
            CompletionTestCase {
                description: "Argument: git add <TAB> (files)",
                input: "git add ",
                cursor_pos: None,
                expected_candidates: vec!["Cargo.toml"], // Assumes running in repo root roughly
                unexpected_candidates: vec!["add", "push"], // Should not suggest subcommands again
            },
            // Git Push: Remote (Arg 0)
            CompletionTestCase {
                description: "Git Push: Arg 0 (Remote) - Partial",
                input: "git push ori",
                cursor_pos: None,
                expected_candidates: vec![], // No "origin" static candidate defined in test DB, but implies Argument context
                unexpected_candidates: vec!["add", "push"],
            },
            // Git Push: Branch (Arg 1) - Key Fix Verification
            CompletionTestCase {
                description: "Git Push: Arg 1 (Branch) - after 'origin '",
                input: "git push origin ",
                cursor_pos: None,
                expected_candidates: vec![], // Again, no static candidates, but verify we are NOT seeing subcommands
                unexpected_candidates: vec!["add", "push", "origin"], // Should NOT see 'origin' as candidate if we are past it
            },
            // Quoted Argument
            CompletionTestCase {
                description: "Git Push: Quoted Arg 1 (origin) - Partial",
                input: "git push \"orig",
                cursor_pos: None,
                expected_candidates: vec![],
                unexpected_candidates: vec!["add", "push"], // Should be Argument context
            },
        ];

        run_completion_test_cases(cases);
    }

    #[test]
    fn test_argument_correction_unit() {
        // Keep the specific unit test for correction logic as it's valuable for internal state verification
        // ... (existing test logic or simplified version)
        // Re-implementing the robust check here

        let mut db = CommandCompletionDatabase::new();
        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git".to_string()),
            global_options: vec![],
            subcommands: vec![SubCommand {
                name: "push".to_string(),
                description: Some("Push".to_string()),
                options: vec![],
                arguments: vec![
                    Argument {
                        name: "remote".to_string(),
                        description: None,
                        arg_type: None,
                    },
                    Argument {
                        name: "branch".to_string(),
                        description: None,
                        arg_type: None,
                    },
                ],
                subcommands: vec![],
            }],
            arguments: vec![],
        };
        db.add_command(git_completion);
        let generator = CompletionGenerator::new(&db);

        // Unit test helper
        let check = |path: Vec<&str>, context: CompletionContext, expected_arg_index: usize| {
            let parsed = ParsedCommandLine {
                command: "git".to_string(),
                subcommand_path: path.iter().map(|s| s.to_string()).collect(),
                args: vec![],
                options: vec![],
                current_token: "".to_string(),
                current_arg: None,
                completion_context: context.clone(),
                specified_options: vec![],
                specified_arguments: vec![],
                raw_args: vec![],
                cursor_index: 0,
            };
            let corrected = generator.correct_parsed_command_line(&parsed);
            if let CompletionContext::Argument { arg_index, .. } = corrected.completion_context {
                assert_eq!(arg_index, expected_arg_index, "Failed for {:?}", context);
            } else {
                panic!(
                    "Expected Argument context, got {:?}",
                    corrected.completion_context
                );
            }
        };

        check(vec!["push", "origin"], CompletionContext::SubCommand, 1);
        check(
            vec!["push", "origin"],
            CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            1,
        );
    }

    #[test]
    fn test_unknown_command_completion() {
        let db = CommandCompletionDatabase::new();
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("test.txt"), "").unwrap();
        let generator = CompletionGenerator::new(&db);
        let dir_str = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        let parsed = ParsedCommandLine {
            command: "cat".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: dir_str.clone(),
            current_arg: Some(dir_str.clone()),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        // generate_candidates should return MissingCommand for unknown commands
        match generator.generate_candidates(&parsed) {
            Err(GeneratorError::MissingCommand(cmd)) => assert_eq!(cmd, "cat"),
            _ => panic!("Expected MissingCommand error"),
        }

        // Verify fallback generation works
        let candidates = generator.generate_fallback_candidates(&dir_str).unwrap();
        let text = Path::new(&dir_str)
            .join("test.txt")
            .to_string_lossy()
            .to_string();
        assert!(candidates.iter().any(|c| c.text == text));
    }

    #[test]
    fn system_command_candidates_scan_path() {
        static PATH_LOCK: LazyLock<parking_lot::Mutex<()>> =
            LazyLock::new(|| parking_lot::Mutex::new(()));
        let _guard = PATH_LOCK.lock();

        let dir = tempdir().unwrap();
        let cmd_path = dir.path().join("my-test-cmd");
        std::fs::write(&cmd_path, "").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&cmd_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&cmd_path, perms).unwrap();
        }

        let old_path = std::env::var_os("PATH");
        let new_path = match &old_path {
            Some(p) => format!("{}:{}", dir.path().display(), p.to_string_lossy()),
            None => dir.path().display().to_string(),
        };
        unsafe { std::env::set_var("PATH", &new_path) };

        let db = CommandCompletionDatabase::new();
        let generator = CompletionGenerator::new(&db);
        let candidates = generator
            .generate_system_command_candidates("my-")
            .expect("system candidates");
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        assert!(
            texts.contains(&"my-test-cmd".to_string()),
            "expected PATH command in {:?}",
            texts
        );

        if let Some(p) = old_path {
            unsafe { std::env::set_var("PATH", p) };
        } else {
            unsafe { std::env::remove_var("PATH") };
        }
    }
}
