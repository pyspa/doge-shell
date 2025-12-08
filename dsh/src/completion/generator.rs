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
    pub fn generate_candidates(
        &self,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        match &parsed.completion_context {
            CompletionContext::Command => self.generate_command_candidates(&parsed.current_token),
            CompletionContext::SubCommand => self.generate_subcommand_candidates(parsed),
            CompletionContext::ShortOption => self.generate_short_option_candidates(parsed),
            CompletionContext::LongOption => self.generate_long_option_candidates(parsed),
            CompletionContext::OptionValue {
                option_name: _,
                value_type,
            } => self.generate_option_value_candidates(parsed, value_type.as_ref()),
            CompletionContext::Argument {
                arg_index: _,
                arg_type,
            } => self.generate_argument_candidates(parsed, arg_type.as_ref()),
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
                // Top-level subcommands
                let mut added_subcommands = false;
                for subcommand in &command_completion.subcommands {
                    if subcommand.name.starts_with(&parsed.current_token) {
                        candidates.push(CompletionCandidate::subcommand(
                            subcommand.name.clone(),
                            subcommand.description.clone(),
                        ));
                        added_subcommands = true;
                    }
                }

                // If no subcommands exist (or match), try to find arguments or global options
                if !added_subcommands {
                    // Check if we should suggest arguments
                    // Only if we haven't exceeded the number of arguments
                    let arg_index = parsed.specified_arguments.len();
                    if arg_index < command_completion.arguments.len() {
                        let arg_def = &command_completion.arguments[arg_index];
                        // If current token technically matches a subcommand (in parser's view) but we have no subcommands,
                        // it might be a value for this argument.
                        let arg_candidates = self.generate_candidates_for_type(
                            arg_def.arg_type.as_ref().unwrap_or(&ArgumentType::String),
                            &parsed.current_token,
                        )?;
                        candidates.extend(arg_candidates);
                    }

                    // Also show global options
                    if !command_completion.global_options.is_empty() {
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
            }
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
            candidates.extend(self.generate_candidates_for_type(arg_type, &parsed.current_token)?);
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
            candidates.extend(self.generate_candidates_for_type(arg_type, &parsed.current_token)?);
        } else {
            // Default to file completion
            candidates.extend(self.generate_file_candidates(&parsed.current_token)?);
        }

        Ok(candidates)
    }

    /// Generate completion candidates based on type
    fn generate_candidates_for_type(
        &self,
        arg_type: &ArgumentType,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        match arg_type {
            ArgumentType::File { extensions } => {
                self.generate_file_candidates_with_filter(current_token, extensions.as_ref())
            }
            ArgumentType::Directory => self.generate_directory_candidates(current_token),
            ArgumentType::Choice(choices) => Ok(choices
                .iter()
                .filter(|choice| choice.starts_with(current_token))
                .map(|choice| CompletionCandidate::argument(choice.clone(), None))
                .collect()),
            ArgumentType::Command => self.generate_system_command_candidates(current_token),
            ArgumentType::Environment => {
                self.generate_environment_variable_candidates(current_token)
            }
            _ => Ok(Vec::new()),
        }
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
}
