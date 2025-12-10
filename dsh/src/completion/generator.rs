use super::command::{
    ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption, CompletionCandidate,
    SubCommand,
};
use super::parser::{CompletionContext, ParsedCommandLine};
use anyhow::Result;
use std::fs;
use std::path::{MAIN_SEPARATOR, Path};

/// Completion candidate generator
pub struct CompletionGenerator {
    /// Command completion database
    database: CommandCompletionDatabase,
}

impl CompletionGenerator {
    /// Create a new generator
    pub fn new(database: CommandCompletionDatabase) -> Self {
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

    /// Generate completion candidates from parsed command line
    ///
    /// This method also corrects the parsed command line if the parser
    /// incorrectly identified arguments as subcommands.
    pub fn correct_parsed_command_line(&self, parsed: &ParsedCommandLine) -> ParsedCommandLine {
        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let mut valid_subcommands = Vec::new();
            let mut invalid_subcommands = Vec::new();
            let mut current_subcommands = &command_completion.subcommands;

            let mut path_valid = true;
            for sub_name in &parsed.subcommand_path {
                if path_valid {
                    if let Some(sub) = current_subcommands.iter().find(|s| &s.name == sub_name) {
                        valid_subcommands.push(sub_name.clone());
                        current_subcommands = &sub.subcommands;
                    } else {
                        path_valid = false;
                        invalid_subcommands.push(sub_name.clone());
                    }
                } else {
                    invalid_subcommands.push(sub_name.clone());
                }
            }

            if !invalid_subcommands.is_empty() {
                let mut new_parsed = parsed.clone();
                new_parsed.subcommand_path = valid_subcommands;

                // Move invalid subcommands to arguments
                // Prepend them to existing arguments
                let mut new_args = invalid_subcommands;
                new_args.extend(new_parsed.specified_arguments);
                new_parsed.specified_arguments = new_args.clone();
                new_parsed.args = new_args.clone();

                // Recalculate completion context
                // We must recalculate if we changed specified_arguments, regardless of previous context
                // Logic: if we moved subcommands to args, we are definitely in Argument context (or should be)
                // UNLESS we are at the very beginning of a new argument?
                // Actually, if we appended to specified_arguments, we are modifying the argument list.

                let arg_index = new_parsed.specified_arguments.len().saturating_sub(
                    if new_parsed
                        .specified_arguments
                        .contains(&new_parsed.current_token)
                    {
                        1
                    } else {
                        0
                    },
                );
                new_parsed.completion_context = CompletionContext::Argument {
                    arg_index,
                    arg_type: None,
                };

                return new_parsed;
            }
        }

        parsed.clone()
    }

    /// Generate completion candidates from parsed command line
    pub fn generate_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
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
    fn generate_command_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
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
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let current_subcommand =
                self.find_current_subcommand(command_completion, &parsed.subcommand_path);

            if let Some(subcommand) = current_subcommand {
                // Nested subcommand candidates
                for sub in &subcommand.subcommands {
                    if sub.name.starts_with(&parsed.current_token) {
                        candidates.push(CompletionCandidate::subcommand(
                            sub.name.clone(),
                            sub.description.clone(),
                        ));
                    }
                }
            } else {
                // Match subcommands
                for subcommand in &command_completion.subcommands {
                    if subcommand.name.starts_with(&parsed.current_token) {
                        candidates.push(CompletionCandidate::subcommand(
                            subcommand.name.clone(),
                            subcommand.description.clone(),
                        ));
                    }
                }

                // ALWAYS check if we should suggest arguments (files etc.) explanation:
                // User wants file completion to work even if subcommands exist (e.g. `git <TAB>` showing files).
                // Only if we haven't exceeded the number of arguments
                let arg_index = parsed.specified_arguments.len();
                if arg_index < command_completion.arguments.len() {
                    let arg_def = &command_completion.arguments[arg_index];
                    let arg_candidates = self.generate_candidates_for_type(
                        arg_def.arg_type.as_ref().unwrap_or(&ArgumentType::String),
                        parsed,
                    )?;
                    candidates.extend(arg_candidates);
                }

                // Also show global options, BUT ONLY IF token starts with "-"
                // This prevents polluting the completion list when the user hasn't typed a dash yet.
                if !command_completion.global_options.is_empty()
                    && parsed.current_token.starts_with('-')
                {
                    for option in &command_completion.global_options {
                        if let Some(ref short) = option.short
                            && short.starts_with(&parsed.current_token)
                        {
                            candidates.push(CompletionCandidate::short_option(
                                short.clone(),
                                option.description.clone(),
                            ));
                        }
                        if let Some(ref long) = option.long
                            && long.starts_with(&parsed.current_token)
                        {
                            candidates.push(CompletionCandidate::long_option(
                                long.clone(),
                                option.description.clone(),
                            ));
                        }
                    }
                }
            }
        } else {
            // Fallback for unknown commands: suggest files by default
            candidates.extend(self.generate_file_candidates(&parsed.current_token)?);
        }

        Ok(candidates)
    }

    /// Generate short option completion candidates
    fn generate_short_option_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let options =
                self.collect_available_options(command_completion, &parsed.subcommand_path);

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
        }

        Ok(candidates)
    }

    /// Generate long option completion candidates
    fn generate_long_option_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let options =
                self.collect_available_options(command_completion, &parsed.subcommand_path);

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
            }
        }

        Ok(candidates)
    }

    /// Generate option value completion candidates
    fn generate_option_value_candidates(
        &self,
        parsed: &ParsedCommandLine,
        value_type: Option<&ArgumentType>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        // Get actual value type
        let actual_value_type = value_type;

        if let Some(arg_type) = actual_value_type {
            candidates.extend(self.generate_candidates_for_type(arg_type, parsed)?);
        }

        Ok(candidates)
    }

    /// Generate argument completion candidates
    fn generate_argument_candidates(
        &self,
        parsed: &ParsedCommandLine,
        arg_type: Option<&ArgumentType>,
    ) -> Result<Vec<CompletionCandidate>> {
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
                candidates.extend(self.generate_file_candidates(&parsed.current_token)?);
            }
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
            ArgumentType::File { extensions } => self
                .generate_file_candidates_with_filter(&parsed.current_token, extensions.as_ref()),
            ArgumentType::Directory => self.generate_directory_candidates(&parsed.current_token),
            ArgumentType::Choice(choices) => Ok(choices
                .iter()
                .filter(|choice| choice.starts_with(&parsed.current_token))
                .map(|choice| CompletionCandidate::argument(choice.clone(), None))
                .collect()),
            ArgumentType::Command => self.generate_system_command_candidates(&parsed.current_token),
            ArgumentType::Environment => {
                self.generate_environment_variable_candidates(&parsed.current_token)
            }
            ArgumentType::Script(command) => self.generate_script_candidates(command, parsed),
            _ => Ok(Vec::new()),
        }
    }

    /// Generate completion candidates by executing a shell script
    fn generate_script_candidates(
        &self,
        command_template: &str,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        // Simple variable substitution
        let mut command = command_template.to_string();
        command = command.replace("$COMMAND", &parsed.command);
        if let Some(arg) = &parsed.current_arg {
            command = command.replace("$CURRENT_TOKEN", arg);
        } else {
            command = command.replace("$CURRENT_TOKEN", "");
        }
        if let Some(first_sub) = parsed.subcommand_path.first() {
            command = command.replace("$SUBCOMMAND", first_sub);
        } else {
            command = command.replace("$SUBCOMMAND", "");
        }

        // Execute command
        // eprintln!("Executing script command: '{}'", command);
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()?;

        if !output.status.success() {
            //eprintln!("Script failed with status: {}", output.status);
            //let stderr = String::from_utf8_lossy(&output.stderr);
            //eprintln!("Script stderr: {}", stderr);
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        //eprintln!("Script stdout: '{}'", stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && trimmed.starts_with(&parsed.current_token) {
                candidates.push(CompletionCandidate::argument(trimmed.to_string(), None));
            } else {
                //eprintln!(
                //    "Skipping line '{}' (token: '{}')",
                //    trimmed, parsed.current_token
                // ;
            }
        }
        Ok(candidates)
    }

    /// Generate file completion candidates
    fn generate_file_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        self.generate_file_candidates_with_filter(current_token, None)
    }

    /// Generate file completion candidates with filter
    fn generate_file_candidates_with_filter(
        &self,
        current_token: &str,
        extensions: Option<&Vec<String>>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(32);

        let (dir_path, file_prefix) = Self::split_dir_and_prefix(current_token);

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with(&file_prefix) {
                    let path = entry.path();

                    // Extension filter
                    if let Some(exts) = extensions
                        && path.is_file()
                    {
                        if let Some(ext) = path.extension() {
                            let ext_str = format!(".{}", ext.to_string_lossy());
                            if !exts.contains(&ext_str) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }

                    let full_path = Self::build_candidate_path(&dir_path, &file_name);

                    if path.is_dir() {
                        candidates.push(CompletionCandidate::directory(full_path));
                    } else {
                        candidates.push(CompletionCandidate::file(full_path));
                    }
                }
            }
        }

        Ok(candidates)
    }

    /// Generate directory completion candidates
    fn generate_directory_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        let (dir_path, dir_prefix) = Self::split_dir_and_prefix(current_token);

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with(&dir_prefix) && entry.path().is_dir() {
                    let full_path = Self::build_candidate_path(&dir_path, &file_name);

                    candidates.push(CompletionCandidate::directory(full_path));
                }
            }
        }

        Ok(candidates)
    }

    fn build_candidate_path(dir_path: &str, file_name: &str) -> String {
        if dir_path == "." || dir_path.is_empty() {
            return file_name.to_string();
        }

        Path::new(dir_path)
            .join(file_name)
            .to_string_lossy()
            .to_string()
    }

    /// Generate system command completion candidates (simplified version)
    fn generate_system_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        // List of common commands (in actual implementation, get from PATH)
        let common_commands = [
            "ls", "cd", "pwd", "mkdir", "rmdir", "rm", "cp", "mv", "cat", "less", "more", "grep",
            "find", "which", "whereis", "man", "help", "echo", "printf", "git", "cargo", "rustc",
            "npm", "node", "python", "python3", "pip", "docker", "kubectl", "ssh", "scp", "curl",
            "wget", "tar", "zip", "unzip",
        ];

        for cmd in &common_commands {
            if cmd.starts_with(current_token) {
                candidates.push(CompletionCandidate::subcommand(cmd.to_string(), None));
            }
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

    /// Find current subcommand
    fn find_current_subcommand<'a>(
        &self,
        command_completion: &'a CommandCompletion,
        subcommand_path: &[String],
    ) -> Option<&'a SubCommand> {
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

    /// Collect available options
    fn collect_available_options<'a>(
        &self,
        command_completion: &'a CommandCompletion,
        subcommand_path: &[String],
    ) -> Vec<&'a CommandOption> {
        let mut options = Vec::new();

        // Global options
        options.extend(&command_completion.global_options);

        // Subcommand options
        if let Some(subcommand) = self.find_current_subcommand(command_completion, subcommand_path)
        {
            options.extend(&subcommand.options);
        }

        options
    }

    fn split_dir_and_prefix(current_token: &str) -> (String, String) {
        if current_token.is_empty() {
            return (".".to_string(), String::new());
        }

        let path = Path::new(current_token);

        if Self::ends_with_path_separator(current_token) {
            let dir = Self::normalize_dir_path(path);
            return (dir, String::new());
        }

        if let Some(parent) = path.parent() {
            let dir = Self::normalize_dir_path(parent);
            let prefix = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            return (dir, prefix);
        }

        (".".to_string(), current_token.to_string())
    }

    fn ends_with_path_separator(token: &str) -> bool {
        token.ends_with(MAIN_SEPARATOR)
            || (MAIN_SEPARATOR != '/' && token.ends_with('/'))
            || (MAIN_SEPARATOR != '\\' && token.ends_with('\\'))
    }

    fn normalize_dir_path(path: &Path) -> String {
        if path.as_os_str().is_empty() {
            ".".to_string()
        } else {
            path.to_string_lossy().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::command::{Argument, CommandCompletion, SubCommand};
    use std::path::Path;
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
        let generator = CompletionGenerator::new(db);

        let candidates = generator.generate_command_candidates("gi").unwrap();
        assert!(!candidates.is_empty());

        let git_candidate = candidates.iter().find(|c| c.text == "git");
        assert!(git_candidate.is_some());
    }

    #[test]
    fn test_generate_subcommand_candidates() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(db);

        let parsed = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec![],
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
    fn build_candidate_path_handles_root_without_double_slash() {
        assert_eq!(
            CompletionGenerator::build_candidate_path("/", "tmp"),
            "/tmp"
        );
    }

    #[test]
    fn build_candidate_path_preserves_relative_paths() {
        assert_eq!(CompletionGenerator::build_candidate_path(".", "foo"), "foo");
        assert_eq!(
            CompletionGenerator::build_candidate_path("/usr", "bin"),
            "/usr/bin"
        );
    }

    #[test]
    fn file_candidates_expand_directory_with_trailing_separator() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("alpha.txt"), "").unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();

        let generator = CompletionGenerator::new(CommandCompletionDatabase::new());
        let token = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        let candidates = generator
            .generate_file_candidates(&token)
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
    fn directory_candidates_expand_directory_with_trailing_separator() {
        let temp = tempdir().unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();

        let generator = CompletionGenerator::new(CommandCompletionDatabase::new());
        let token = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        let candidates = generator
            .generate_directory_candidates(&token)
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

        let generator = CompletionGenerator::new(db);

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

        let generator = CompletionGenerator::new(db);
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

        let generator = CompletionGenerator::new(db);
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
        let generator = CompletionGenerator::new(db);

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
        let generator = CompletionGenerator::new(db);

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
            cursor_index: 0,
        };

        let candidates = generator
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
        let generator2 = CompletionGenerator::new(db2);

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
            }],
            subcommands: vec![],
            arguments: vec![],
        };
        db.add_command(mycmd_completion);

        let generator = CompletionGenerator::new(db);

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
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
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

    #[test]
    fn test_git_push_argument_correction() {
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
        let generator = CompletionGenerator::new(db);

        // Helper to check correction logic
        let check_correction = |path: Vec<&str>,
                                context: CompletionContext,
                                expected_index: usize| {
            let parsed = ParsedCommandLine {
                command: "git".to_string(),
                subcommand_path: path.iter().map(|s| s.to_string()).collect(),
                args: vec![],
                options: vec![],
                current_token: "".to_string(),
                current_arg: None,
                completion_context: context.clone(),
                specified_options: vec![],
                specified_arguments: vec![], // Start empty, let correction fill it
                cursor_index: 0,
            };

            let corrected = generator.correct_parsed_command_line(&parsed);

            assert_eq!(corrected.subcommand_path, vec!["push".to_string()]);
            assert_eq!(corrected.specified_arguments, vec!["origin".to_string()]);

            if let CompletionContext::Argument { arg_index, .. } = corrected.completion_context {
                assert_eq!(
                    arg_index, expected_index,
                    "Failed for context {:?}",
                    context
                );
            } else {
                panic!(
                    "Expected Argument context, got {:?} for input context {:?}",
                    corrected.completion_context, context
                );
            }
        };

        // Case 1: parser thinks origin is subcommand (SubCommand context)
        check_correction(vec!["push", "origin"], CompletionContext::SubCommand, 1);

        // Case 2: parser thinks origin is subcommand BUT returns Argument context
        // (e.g. because of trailing space or empty token not matching subcommands)
        check_correction(
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
        // Database is empty, so "cat" or any command is unknown.

        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("test.txt"), "").unwrap();

        let generator = CompletionGenerator::new(db);
        let dir_str = format!("{}{}", temp.path().display(), MAIN_SEPARATOR);

        // Mock: "cat [dir]/"
        // Expect: "test.txt" (file) because "cat" is unknown -> fallback to file completion
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
            cursor_index: 0,
        };

        let candidates = generator.generate_candidates(&parsed).unwrap();
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        let expected_file = Path::new(&dir_str)
            .join("test.txt")
            .to_string_lossy()
            .to_string();

        assert!(
            texts.contains(&expected_file),
            "Should contain file candidate for unknown command: {:?}",
            texts
        );
    }
}
