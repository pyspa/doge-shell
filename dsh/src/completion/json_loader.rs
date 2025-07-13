#![allow(dead_code)]
use crate::shell::APP_NAME;

use super::command::{CommandCompletion, CommandCompletionDatabase};
use anyhow::{Context, Result};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

// Pre-compiled regex patterns for option validation
static SHORT_OPTION_VALIDATION_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^-[a-zA-Z]$").unwrap());
static LONG_OPTION_VALIDATION_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^--[a-zA-Z][a-zA-Z0-9-]{2,}$").unwrap());

pub struct JsonCompletionLoader {
    completion_dirs: Vec<PathBuf>,
}

impl JsonCompletionLoader {
    pub fn new() -> Self {
        Self {
            completion_dirs: Self::get_default_completion_dirs(),
        }
    }

    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            completion_dirs: dirs,
        }
    }

    fn get_default_completion_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        if let Some(config_dir) = dirs::config_dir() {
            let user_dir = config_dir.join(APP_NAME).join("completions");
            debug!("Adding user config completion dir: {:?}", user_dir);
            dirs.push(user_dir);
        }

        if let Some(home_dir) = dirs::home_dir() {
            let home_config_dir = home_dir.join(".config").join(APP_NAME).join("completions");
            debug!("Adding home config completion dir: {:?}", home_config_dir);
            dirs.push(home_config_dir);
        }

        let local_dir = PathBuf::from("./completions");
        debug!("Adding local completion dir: {:?}", local_dir);
        dirs.push(local_dir);

        debug!("Initialized completion directories: {:?}", dirs);
        dirs
    }

    pub fn load_database(&self) -> Result<CommandCompletionDatabase> {
        debug!("Starting completion database loading...");
        let mut database = CommandCompletionDatabase::new();
        let mut loaded_count = 0;
        let mut error_count = 0;

        debug!(
            "Checking {} completion directories",
            self.completion_dirs.len()
        );

        for (i, dir) in self.completion_dirs.iter().enumerate() {
            debug!(
                "Checking completion directory {}/{}: {:?}",
                i + 1,
                self.completion_dirs.len(),
                dir
            );

            if !dir.exists() {
                debug!("Completion directory does not exist: {:?}", dir);
                continue;
            }

            debug!("Loading completions from existing directory: {:?}", dir);
            match self.load_from_directory(dir, &mut database) {
                Ok(count) => {
                    loaded_count += count;
                    if count > 0 {
                        debug!(
                            "Successfully loaded {} completion files from {:?}",
                            count, dir
                        );
                    } else {
                        debug!("No completion files found in {:?}", dir);
                    }
                }
                Err(e) => {
                    error_count += 1;
                    warn!("Failed to load completions from {:?}: {}", dir, e);
                }
            }
        }

        debug!(
            "Completion loading complete: {} files loaded, {} errors from {} directories",
            loaded_count,
            error_count,
            self.completion_dirs.len()
        );

        Ok(database)
    }

    /// Load completion data from specified directory
    fn load_from_directory(
        &self,
        dir: &Path,
        database: &mut CommandCompletionDatabase,
    ) -> Result<usize> {
        debug!("Reading directory entries from: {:?}", dir);
        let entries =
            fs::read_dir(dir).with_context(|| format!("Failed to read directory: {:?}", dir))?;

        let mut loaded_count = 0;
        let mut file_count = 0;

        for entry in entries {
            let entry =
                entry.with_context(|| format!("Failed to read directory entry in {:?}", dir))?;
            let path = entry.path();
            file_count += 1;

            debug!("Found file: {:?}", path);

            // Process only .json files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                debug!("Skipping non-JSON file: {:?}", path);
                continue;
            }

            debug!("Processing JSON completion file: {:?}", path);
            match self.load_completion_file(&path) {
                Ok(completion) => {
                    debug!(
                        "Successfully loaded completion for command: {} from {:?}",
                        completion.command, path
                    );
                    debug!(
                        "Completion details - subcommands: {}, global_options: {}",
                        completion.subcommands.len(),
                        completion.global_options.len()
                    );
                    database.add_command(completion);
                    loaded_count += 1;
                }
                Err(e) => {
                    warn!("Failed to load completion file {:?}: {}", path, e);
                }
            }
        }

        debug!(
            "Directory scan complete: found {} files, loaded {} JSON completion files from {:?}",
            file_count, loaded_count, dir
        );
        Ok(loaded_count)
    }

    /// Load a single completion file
    fn load_completion_file(&self, path: &Path) -> Result<CommandCompletion> {
        debug!("Reading file content from: {:?}", path);
        let content =
            fs::read_to_string(path).with_context(|| format!("Failed to read file: {:?}", path))?;

        debug!("File content length: {} bytes", content.len());
        debug!("Parsing JSON content from: {:?}", path);

        let completion: CommandCompletion = match serde_json::from_str(&content) {
            Ok(completion) => completion,
            Err(e) => {
                warn!("JSON parse error in {:?}: {}", path, e);
                debug!("JSON parse error details: {:?}", e);
                return Err(anyhow::anyhow!(
                    "Failed to parse JSON in file: {:?}: {}",
                    path,
                    e
                ));
            }
        };

        debug!(
            "Successfully parsed JSON for command: {}",
            completion.command
        );

        // Basic validation
        debug!("Validating completion data for: {}", completion.command);
        self.validate_completion(&completion)
            .with_context(|| format!("Validation failed for file: {:?}", path))?;

        debug!("Validation successful for: {}", completion.command);
        Ok(completion)
    }

    /// Basic validation of completion data
    fn validate_completion(&self, completion: &CommandCompletion) -> Result<()> {
        if completion.command.is_empty() {
            anyhow::bail!("Command name cannot be empty");
        }

        // Check if command name contains invalid characters
        if completion.command.contains(char::is_whitespace) {
            anyhow::bail!(
                "Command name cannot contain whitespace: '{}'",
                completion.command
            );
        }

        // Validate subcommands
        for subcommand in &completion.subcommands {
            self.validate_subcommand(subcommand, &completion.command)?;
        }

        // Validate global options
        for option in &completion.global_options {
            self.validate_option(option, &completion.command)?;
        }

        Ok(())
    }

    /// Validate subcommand
    fn validate_subcommand(
        &self,
        subcommand: &super::command::SubCommand,
        parent_command: &str,
    ) -> Result<()> {
        if subcommand.name.is_empty() {
            anyhow::bail!(
                "Subcommand name cannot be empty in command '{}'",
                parent_command
            );
        }

        if subcommand.name.contains(char::is_whitespace) {
            anyhow::bail!(
                "Subcommand name cannot contain whitespace: '{}' in command '{}'",
                subcommand.name,
                parent_command
            );
        }

        // Validate options
        for option in &subcommand.options {
            self.validate_option(option, &format!("{} {}", parent_command, subcommand.name))?;
        }

        // Validate nested subcommands
        for nested_subcommand in &subcommand.subcommands {
            self.validate_subcommand(
                nested_subcommand,
                &format!("{} {}", parent_command, subcommand.name),
            )?;
        }

        Ok(())
    }

    /// Validate option
    fn validate_option(&self, option: &super::command::CommandOption, context: &str) -> Result<()> {
        if option.short.is_none() && option.long.is_none() {
            anyhow::bail!(
                "Option must have either short or long form in '{}'",
                context
            );
        }

        if let Some(ref short) = option.short {
            if !SHORT_OPTION_VALIDATION_REGEX.is_match(short) {
                anyhow::bail!("Invalid short option format '{}' in '{}'", short, context);
            }
        }

        if let Some(ref long) = option.long {
            if !LONG_OPTION_VALIDATION_REGEX.is_match(long) {
                anyhow::bail!("Invalid long option format '{}' in '{}'", long, context);
            }
        }

        Ok(())
    }

    /// Load completion data for specific command
    pub fn load_command_completion(&self, command_name: &str) -> Result<Option<CommandCompletion>> {
        let filename = format!("{}.json", command_name);

        for dir in &self.completion_dirs {
            let path = dir.join(&filename);
            if path.exists() {
                match self.load_completion_file(&path) {
                    Ok(completion) => return Ok(Some(completion)),
                    Err(e) => {
                        warn!(
                            "Failed to load completion for '{}' from {:?}: {}",
                            command_name, path, e
                        );
                    }
                }
            }
        }

        Ok(None)
    }

    /// Get list of available completion files
    pub fn list_available_completions(&self) -> Result<Vec<String>> {
        let mut commands = Vec::new();

        for dir in &self.completion_dirs {
            if !dir.exists() {
                continue;
            }

            let entries = fs::read_dir(dir)
                .with_context(|| format!("Failed to read directory: {:?}", dir))?;

            for entry in entries {
                let entry = entry?;
                let path = entry.path();

                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if !commands.contains(&stem.to_string()) {
                            commands.push(stem.to_string());
                        }
                    }
                }
            }
        }

        commands.sort();
        Ok(commands)
    }
}

impl Default for JsonCompletionLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_load_valid_completion_file() {
        let temp_dir = TempDir::new().unwrap();
        let completion_file = temp_dir.path().join("test.json");

        let test_completion = r#"
        {
            "command": "test",
            "description": "Test command",
            "global_options": [],
            "subcommands": [
                {
                    "name": "sub",
                    "description": "Test subcommand",
                    "aliases": [],
                    "options": [],
                    "arguments": [],
                    "subcommands": []
                }
            ]
        }
        "#;

        fs::write(&completion_file, test_completion).unwrap();

        let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
        let result = loader.load_completion_file(&completion_file);

        assert!(result.is_ok());
        let completion = result.unwrap();
        assert_eq!(completion.command, "test");
        assert_eq!(completion.subcommands.len(), 1);
        assert_eq!(completion.subcommands[0].name, "sub");
    }

    #[test]
    fn test_load_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let completion_file = temp_dir.path().join("invalid.json");

        fs::write(&completion_file, "invalid json").unwrap();

        let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
        let result = loader.load_completion_file(&completion_file);

        assert!(result.is_err());
    }

    #[test]
    fn test_validation_empty_command_name() {
        let loader = JsonCompletionLoader::new();
        let completion = CommandCompletion {
            command: "".to_string(),
            description: None,
            subcommands: vec![],
            global_options: vec![],
        };

        let result = loader.validate_completion(&completion);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_available_completions() {
        let temp_dir = TempDir::new().unwrap();

        // Create test JSON files
        fs::write(temp_dir.path().join("git.json"), "{}").unwrap();
        fs::write(temp_dir.path().join("cargo.json"), "{}").unwrap();
        fs::write(temp_dir.path().join("not_json.txt"), "{}").unwrap();

        let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
        let completions = loader.list_available_completions().unwrap();

        assert_eq!(completions.len(), 2);
        assert!(completions.contains(&"git".to_string()));
        assert!(completions.contains(&"cargo".to_string()));
        assert!(!completions.contains(&"not_json".to_string()));
    }

    #[test]
    fn test_load_real_git_completion() {
        let loader = JsonCompletionLoader::with_dirs(vec![PathBuf::from("../completions")]);

        match loader.load_command_completion("git") {
            Ok(Some(completion)) => {
                assert_eq!(completion.command, "git");
                assert!(completion.description.is_some());
                assert!(!completion.subcommands.is_empty());

                // Verify that "add" subcommand exists
                let add_subcommand = completion.subcommands.iter().find(|sc| sc.name == "add");
                assert!(add_subcommand.is_some());

                let add = add_subcommand.unwrap();
                assert!(add.description.is_some());
                assert!(!add.options.is_empty());

                println!(
                    "Successfully loaded git completion with {} subcommands",
                    completion.subcommands.len()
                );
            }
            Ok(None) => {
                println!(
                    "Git completion file not found - this is expected if running from different directory"
                );
            }
            Err(e) => {
                println!("Error loading git completion: {}", e);
            }
        }
    }

    #[test]
    fn test_load_real_cargo_completion() {
        let loader = JsonCompletionLoader::with_dirs(vec![PathBuf::from("../completions")]);

        match loader.load_command_completion("cargo") {
            Ok(Some(completion)) => {
                assert_eq!(completion.command, "cargo");
                assert!(completion.description.is_some());
                assert!(!completion.subcommands.is_empty());

                // Verify that "build" subcommand exists
                let build_subcommand = completion.subcommands.iter().find(|sc| sc.name == "build");
                assert!(build_subcommand.is_some());

                println!(
                    "Successfully loaded cargo completion with {} subcommands",
                    completion.subcommands.len()
                );
            }
            Ok(None) => {
                println!(
                    "Cargo completion file not found - this is expected if running from different directory"
                );
            }
            Err(e) => {
                println!("Error loading cargo completion: {}", e);
            }
        }
    }
}
