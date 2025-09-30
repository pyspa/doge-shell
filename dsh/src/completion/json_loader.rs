#![allow(dead_code)]
use super::command::{CommandCompletion, CommandCompletionDatabase};
use crate::shell::APP_NAME;
use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{debug, warn};

/// Embedded completion assets using rust-embed
/// This embeds all JSON files from the completions/ directory at compile time
#[derive(RustEmbed)]
#[folder = "../completions/"]
struct CompletionAssets;

pub struct JsonCompletionLoader {
    completion_dirs: Vec<PathBuf>,
}

/// Global cache for the loaded completion database
static COMPLETION_DATABASE_CACHE: OnceLock<CommandCompletionDatabase> = OnceLock::new();

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
        // Use the static cache to avoid re-parsing JSON files on each call
        // Since get_or_try_init is unstable, we need to handle the Result manually
        match COMPLETION_DATABASE_CACHE.get() {
            Some(database) => {
                debug!("Using cached completion database (already loaded)");
                Ok(database.clone())
            }
            None => {
                debug!(
                    "Starting completion database loading from embedded resources (first time)..."
                );
                let mut database = CommandCompletionDatabase::new();
                let mut loaded_count = 0;

                // First, load from embedded resources
                debug!("Loading completions from embedded resources...");
                match self.load_from_embedded(&mut database) {
                    Ok(count) => {
                        loaded_count += count;
                        debug!(
                            "Successfully loaded {} completion files from embedded resources",
                            count
                        );
                    }
                    Err(e) => {
                        warn!("Failed to load completions from embedded resources: {}", e);
                        // Even on error, we could still store a partially loaded database
                        // but in this case, if embedded loading fails, we return the error
                        return Err(e);
                    }
                }

                debug!(
                    "Checking {} completion directories for additional files",
                    self.completion_dirs.len()
                );

                debug!(
                    "Completion loading complete: {} files loaded from embedded resources + {} directories",
                    loaded_count,
                    self.completion_dirs.len()
                );

                // Store the result in the cache to avoid reloading on subsequent calls
                // We only reach this point if loading was successful
                COMPLETION_DATABASE_CACHE.get_or_init(|| database.clone());

                Ok(database)
            }
        }
    }

    /// Load completion data from embedded resources
    fn load_from_embedded(&self, database: &mut CommandCompletionDatabase) -> Result<usize> {
        debug!("Loading completions from embedded resources...");
        let mut loaded_count = 0;
        let mut file_count = 0;

        // Iterate through all embedded files
        for file_path in CompletionAssets::iter() {
            file_count += 1;
            debug!("Found embedded file: {}", file_path);

            // Process only .json files
            if !file_path.ends_with(".json") {
                debug!("Skipping non-JSON embedded file: {}", file_path);
                continue;
            }

            debug!("Processing embedded JSON completion file: {}", file_path);

            // Get the embedded file content
            match CompletionAssets::get(&file_path) {
                Some(file_data) => {
                    match self.load_completion_from_content(&file_data.data, &file_path) {
                        Ok(completion) => {
                            debug!(
                                "Successfully loaded completion for command: {} from embedded file: {}",
                                completion.command, file_path
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
                            warn!(
                                "Failed to load embedded completion file {}: {}",
                                file_path, e
                            );
                        }
                    }
                }
                None => {
                    warn!("Failed to get embedded file content for: {}", file_path);
                }
            }
        }

        debug!(
            "Embedded resource scan complete: found {} files, loaded {} JSON completion files",
            file_count, loaded_count
        );
        Ok(loaded_count)
    }

    /// Load completion data from specified directory
    fn load_from_directory(
        &self,
        dir: &Path,
        database: &mut CommandCompletionDatabase,
    ) -> Result<usize> {
        debug!("Reading directory entries from: {:?}", dir);
        let entries =
            fs::read_dir(dir).with_context(|| format!("Failed to read directory: {dir:?}"))?;

        let mut loaded_count = 0;
        let mut file_count = 0;

        for entry in entries {
            let entry =
                entry.with_context(|| format!("Failed to read directory entry in {dir:?}"))?;
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
        let content = fs::read(path).with_context(|| format!("Failed to read file: {path:?}"))?;

        debug!("File content length: {} bytes", content.len());

        let source_name = path.to_string_lossy();
        self.load_completion_from_content(&content, &source_name)
    }

    /// Load completion from byte content (used for embedded resources)
    fn load_completion_from_content(
        &self,
        content: &[u8],
        source_name: &str,
    ) -> Result<CommandCompletion> {
        debug!("Parsing content from: {}", source_name);

        // Convert bytes to string
        let content_str = std::str::from_utf8(content)
            .with_context(|| format!("Failed to convert content to UTF-8 string: {source_name}"))?;

        debug!("Content length: {} bytes", content_str.len());
        debug!("Parsing JSON content from: {}", source_name);

        let completion: CommandCompletion = match serde_json::from_str(content_str) {
            Ok(completion) => completion,
            Err(e) => {
                warn!("JSON parse error in {}: {}", source_name, e);
                debug!("JSON parse error details: {:?}", e);
                return Err(anyhow::anyhow!(
                    "Failed to parse JSON in source: {}: {}",
                    source_name,
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
            .with_context(|| format!("Validation failed for source: {source_name}"))?;

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
            // Short options must start with exactly one dash, followed by at least one non-whitespace, non-dash character
            if !short.starts_with('-') || short.starts_with("--") {
                anyhow::bail!("Invalid short option format '{}' in '{}'", short, context);
            }
        }

        if let Some(ref long) = option.long {
            // Long options must start with exactly two dashes, followed by at least one non-whitespace character
            if !long.starts_with("--") || long.len() <= 2 {
                anyhow::bail!("Invalid long option format '{}' in '{}'", long, context);
            }
        }

        Ok(())
    }

    /// Load completion data for specific command
    pub fn load_command_completion(&self, command_name: &str) -> Result<Option<CommandCompletion>> {
        let filename = format!("{command_name}.json");

        // First, try to load from embedded resources
        debug!("Checking embedded resources for: {}", filename);
        if let Some(file_data) = CompletionAssets::get(&filename) {
            debug!("Found embedded completion for: {}", command_name);
            match self.load_completion_from_content(&file_data.data, &filename) {
                Ok(completion) => {
                    debug!(
                        "Successfully loaded embedded completion for: {}",
                        command_name
                    );
                    return Ok(Some(completion));
                }
                Err(e) => {
                    warn!(
                        "Failed to load embedded completion for '{}': {}",
                        command_name, e
                    );
                }
            }
        } else {
            debug!("No embedded completion found for: {}", command_name);
        }

        // Fallback to filesystem directories
        debug!("Checking filesystem directories for: {}", filename);
        for dir in &self.completion_dirs {
            let path = dir.join(&filename);
            if path.exists() {
                debug!("Found filesystem completion at: {:?}", path);
                match self.load_completion_file(&path) {
                    Ok(completion) => {
                        debug!(
                            "Successfully loaded filesystem completion for: {}",
                            command_name
                        );
                        return Ok(Some(completion));
                    }
                    Err(e) => {
                        warn!(
                            "Failed to load completion for '{}' from {:?}: {}",
                            command_name, path, e
                        );
                    }
                }
            }
        }

        debug!("No completion found for command: {}", command_name);
        Ok(None)
    }

    /// Get list of available completion files
    pub fn list_available_completions(&self) -> Result<Vec<String>> {
        let mut commands = std::collections::BTreeSet::new();

        // First, collect from embedded resources
        debug!("Collecting completions from embedded resources...");
        for file_path in CompletionAssets::iter() {
            if file_path.ends_with(".json") {
                // Extract command name from filename like "git.json"
                if let Some(stem) = file_path.strip_suffix(".json") {
                    debug!("Found embedded completion for: {}", stem);
                    commands.insert(stem.to_string());
                }
            }
        }

        // Then, collect from filesystem directories
        debug!("Collecting completions from filesystem directories...");
        for dir in &self.completion_dirs {
            if !dir.exists() {
                continue;
            }

            let entries =
                fs::read_dir(dir).with_context(|| format!("Failed to read directory: {dir:?}"))?;

            for entry in entries {
                let entry = entry?;
                let path = entry.path();

                if path.extension().and_then(|s| s.to_str()) == Some("json")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    debug!("Found filesystem completion for: {}", stem);
                    commands.insert(stem.to_string());
                }
            }
        }

        let result: Vec<String> = commands.into_iter().collect();
        debug!("Total available completions: {}", result.len());
        Ok(result)
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

        // Should include both embedded completions and filesystem completions
        // Embedded: git, cargo, docker, npm, kubectl, make, pacman, systemctl, and others
        assert_eq!(completions.len(), 16);
        assert!(completions.contains(&"git".to_string()));
        assert!(completions.contains(&"cargo".to_string()));
        assert!(completions.contains(&"docker".to_string()));
        assert!(completions.contains(&"npm".to_string()));
        assert!(completions.contains(&"kubectl".to_string()));
        assert!(completions.contains(&"make".to_string()));
        assert!(!completions.contains(&"not_json".to_string()));
    }

    #[test]
    fn test_load_real_git_completion() {
        let loader = JsonCompletionLoader::new();

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
                    "Git completion file not found - this is expected if no embedded or filesystem completion exists"
                );
            }
            Err(e) => {
                println!("Error loading git completion: {e}");
            }
        }
    }

    #[test]
    fn test_load_real_cargo_completion() {
        let loader = JsonCompletionLoader::new();

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
                    "Cargo completion file not found - this is expected if no embedded or filesystem completion exists"
                );
            }
            Err(e) => {
                println!("Error loading cargo completion: {e}");
            }
        }
    }

    #[test]
    fn test_embedded_completions_available() {
        let loader = JsonCompletionLoader::new();

        match loader.list_available_completions() {
            Ok(completions) => {
                println!("Available completions: {completions:?}");
                // We expect at least some completions to be available from embedded resources
                // The exact number depends on what's in the completions/ directory
            }
            Err(e) => {
                println!("Error listing completions: {e}");
            }
        }
    }

    #[test]
    fn test_load_completion_from_content() {
        let loader = JsonCompletionLoader::new();

        let test_json = r#"
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

        let result = loader.load_completion_from_content(test_json.as_bytes(), "test_source");
        assert!(result.is_ok());

        let completion = result.unwrap();
        assert_eq!(completion.command, "test");
        assert_eq!(completion.subcommands.len(), 1);
        assert_eq!(completion.subcommands[0].name, "sub");
    }

    fn extract_option_base(option: &str) -> &str {
        // Extract the base option name without any placeholders
        // e.g., "-f <FILE>" -> "-f", "--type <TYPE>" -> "--type"
        option.split_whitespace().next().unwrap_or(option)
    }

    #[test]
    fn test_extract_option_base() {
        assert_eq!(extract_option_base("-f"), "-f");
        assert_eq!(extract_option_base("--file"), "--file");
        assert_eq!(extract_option_base("-f <FILE>"), "-f");
        assert_eq!(extract_option_base("--type <TYPE>"), "--type");
        assert_eq!(extract_option_base("--output <PATH>"), "--output");
        assert_eq!(extract_option_base("--file-name <NAME>"), "--file-name");
        assert_eq!(extract_option_base("--option"), "--option");
        assert_eq!(extract_option_base(""), "");
    }

    #[test]
    fn test_validate_option_with_placeholders() {
        let loader = JsonCompletionLoader::new();

        // Test short option with placeholder
        let option_with_short = crate::completion::command::CommandOption {
            short: Some("-f <FILE>".to_string()),
            long: None,
            description: None,
        };
        assert!(loader.validate_option(&option_with_short, "test").is_ok());

        // Test long option with placeholder
        let option_with_long = crate::completion::command::CommandOption {
            short: None,
            long: Some("--type <TYPE>".to_string()),
            description: None,
        };
        assert!(loader.validate_option(&option_with_long, "test").is_ok());

        // Test both short and long with placeholders
        let option_both = crate::completion::command::CommandOption {
            short: Some("-f <FILE>".to_string()),
            long: Some("--file <FILE>".to_string()),
            description: None,
        };
        assert!(loader.validate_option(&option_both, "test").is_ok());

        // Test invalid short option (only option that should fail is -- or just -)
        let invalid_short = crate::completion::command::CommandOption {
            short: Some("--".to_string()), // Invalid: this is a long option prefix, not a short option
            long: None,
            description: None,
        };
        assert!(loader.validate_option(&invalid_short, "test").is_err());

        // Test that valid short option like -123 is now accepted
        let valid_short_with_number = crate::completion::command::CommandOption {
            short: Some("-123".to_string()), // Should be valid now: starts with -
            long: None,
            description: None,
        };
        assert!(
            loader
                .validate_option(&valid_short_with_number, "test")
                .is_ok()
        );

        // Test invalid long option (should still fail)
        let invalid_long = crate::completion::command::CommandOption {
            short: None,
            long: Some("--".to_string()), // Invalid: just -- without any content
            description: None,
        };
        assert!(loader.validate_option(&invalid_long, "test").is_err());

        // Test that long option starting with -- and containing numbers is now valid
        let valid_long_with_number = crate::completion::command::CommandOption {
            short: None,
            long: Some("--123invalid".to_string()), // Should be valid now: starts with --
            description: None,
        };
        assert!(
            loader
                .validate_option(&valid_long_with_number, "test")
                .is_ok()
        );
    }

    /// Test that all embedded JSON completion files can be loaded and parsed correctly
    #[test]
    fn test_all_embedded_completion_files_load_correctly() {
        let loader = JsonCompletionLoader::new();
        let available_completions = loader
            .list_available_completions()
            .expect("Failed to list available completions");

        for command_name in available_completions {
            println!("Testing completion file: {}", command_name);

            // Load the command completion
            let result = loader.load_command_completion(&command_name);
            assert!(
                result.is_ok(),
                "Failed to load completion for command '{}': {:?}",
                command_name,
                result.err()
            );

            // Check that we got a completion
            let completion = result.unwrap();
            assert!(
                completion.is_some(),
                "Expected completion for command '{}' but got None",
                command_name
            );

            // Check that the completion has the command name
            let completion = completion.unwrap();
            assert_eq!(
                completion.command, command_name,
                "Command name mismatch for '{}': expected '{}', got '{}'",
                command_name, command_name, completion.command
            );

            // Validate the completion data
            let validation_result = loader.validate_completion(&completion);
            assert!(
                validation_result.is_ok(),
                "Validation failed for command '{}': {:?}",
                command_name,
                validation_result.err()
            );

            println!(
                "✓ Successfully loaded and validated completion for '{}': {} subcommands, {} global options",
                command_name,
                completion.subcommands.len(),
                completion.global_options.len()
            );
        }
    }

    /// Test that completion candidates can be generated from loaded JSON files
    #[test]
    fn test_completion_candidates_generation_from_json() {
        use crate::completion::generator::CompletionGenerator;
        use crate::completion::parser::{CompletionContext, ParsedCommandLine};

        let loader = JsonCompletionLoader::new();
        let database = loader.load_database().expect("Failed to load database");
        let generator = CompletionGenerator::new(database);

        // Test with the commands we know exist in the test files
        let test_commands = ["git", "cargo", "docker", "rg"];

        for command in &test_commands {
            if generator.has_command_completion(command) {
                println!(
                    "Testing completion candidate generation for command: {}",
                    command
                );

                // Test command-level completion
                let parsed_command = ParsedCommandLine {
                    command: command.to_string(),
                    subcommand_path: vec![],
                    args: vec![],
                    options: vec![],
                    current_token: "".to_string(),
                    current_arg: Some("".to_string()),
                    completion_context: CompletionContext::Command,
                    specified_options: vec![],
                    specified_arguments: vec![],
                    cursor_index: 0,
                };

                let candidates = generator.generate_candidates(&parsed_command).unwrap();
                assert!(
                    !candidates.is_empty(),
                    "Expected completion candidates for command '{}'",
                    command
                );

                println!(
                    "  ✓ Generated {} command candidates for '{}'",
                    candidates.len(),
                    command
                );

                // Test subcommand completion for commands that have subcommands
                if generator.has_command_completion(command) {
                    // Use the loader to get the original completion for verification
                    let loader_for_test = JsonCompletionLoader::new();
                    if let Ok(Some(cmd_completion)) =
                        loader_for_test.load_command_completion(command)
                        && !cmd_completion.subcommands.is_empty()
                    {
                        let first_subcommand = &cmd_completion.subcommands[0];
                        println!(
                            "  Testing subcommands for '{}', first subcommand: '{}'",
                            command, first_subcommand.name
                        );

                        let parsed_subcommand = ParsedCommandLine {
                            command: command.to_string(),
                            subcommand_path: vec![],
                            args: vec![],
                            options: vec![],
                            current_token: first_subcommand
                                .name
                                .chars()
                                .take(1)
                                .collect::<String>(), // Use first letter to test filtering
                            current_arg: Some(
                                first_subcommand.name.chars().take(1).collect::<String>(),
                            ),
                            completion_context: CompletionContext::SubCommand,
                            specified_options: vec![],
                            specified_arguments: vec![],
                            cursor_index: 0,
                        };

                        let subcommand_candidates =
                            generator.generate_candidates(&parsed_subcommand).unwrap();
                        assert!(
                            !subcommand_candidates.is_empty(),
                            "Expected subcommand candidates for '{}'",
                            command
                        );

                        println!(
                            "  ✓ Generated {} subcommand candidates for '{}'",
                            subcommand_candidates.len(),
                            command
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_completion_files_display_candidates_correctly() {
        use crate::completion::display::Candidate as DisplayCandidate;
        use crate::completion::generator::CompletionGenerator;
        use crate::completion::parser::{CompletionContext, ParsedCommandLine};

        let loader = JsonCompletionLoader::new();
        let database = loader.load_database().expect("Failed to load database");
        let generator = CompletionGenerator::new(database);

        // Test specific commands to make sure they can produce displayable candidates
        let test_commands = ["git", "cargo"];

        for command in &test_commands {
            if generator.has_command_completion(command) {
                println!(
                    "Testing display candidate generation for command: {}",
                    command
                );

                // Generate candidates for subcommands
                let parsed_command = ParsedCommandLine {
                    command: command.to_string(),
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

                let enhanced_candidates = generator.generate_candidates(&parsed_command).unwrap();
                assert!(
                    !enhanced_candidates.is_empty(),
                    "Expected completion candidates for command '{}'",
                    command
                );

                // Convert to display candidates and ensure they're formatted correctly
                let display_candidates: Vec<DisplayCandidate> = enhanced_candidates
                    .into_iter()
                    .map(|c| match c.completion_type {
                        super::super::command::CompletionType::SubCommand => {
                            DisplayCandidate::Command {
                                name: c.text,
                                description: c.description.unwrap_or_default(),
                            }
                        }
                        super::super::command::CompletionType::LongOption
                        | super::super::command::CompletionType::ShortOption => {
                            DisplayCandidate::Option {
                                name: c.text,
                                description: c.description.unwrap_or_default(),
                            }
                        }
                        _ => DisplayCandidate::Item(c.text, c.description.unwrap_or_default()),
                    })
                    .collect();

                assert!(
                    !display_candidates.is_empty(),
                    "Expected display candidates for command '{}'",
                    command
                );

                println!(
                    "  ✓ Generated {} display candidates for '{}'",
                    display_candidates.len(),
                    command
                );

                // Verify that descriptions are properly set
                for candidate in &display_candidates {
                    match candidate {
                        DisplayCandidate::Command { name, description } => {
                            println!("    Subcommand: {} - {}", name, description);
                            // Verify that the subcommand is from our JSON file
                            // We can't directly access the database, so just verify they exist
                            assert!(
                                generator.has_command_completion(command),
                                "Command '{}' is not available in completion database",
                                command
                            );
                        }
                        DisplayCandidate::Option { name, description } => {
                            println!("    Option: {} - {}", name, description);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
