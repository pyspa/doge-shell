use super::command::{ArgumentType, CommandCompletion, CommandCompletionDatabase};
use crate::shell::APP_NAME;
use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::{debug, warn};

/// Embedded completion assets using rust-embed.
///
/// `dsh/completions/` is the canonical built-in source. User config completion
/// directories are checked before embedded assets so generated definitions can
/// override built-ins; local dev fallback directories are checked after embedded.
#[derive(RustEmbed)]
#[folder = "completions/"]
struct CompletionAssets;

pub struct JsonCompletionLoader {
    override_dirs: Vec<PathBuf>,
    fallback_dirs: Vec<PathBuf>,
}

/// Global cache for the loaded completion database
static COMPLETION_DATABASE_CACHE: OnceLock<Arc<CommandCompletionDatabase>> = OnceLock::new();

impl JsonCompletionLoader {
    pub fn new() -> Self {
        Self {
            override_dirs: Self::get_default_override_dirs(),
            fallback_dirs: Self::get_default_fallback_dirs(),
        }
    }

    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            override_dirs: dirs,
            fallback_dirs: Vec::new(),
        }
    }

    #[cfg(test)]
    fn with_override_and_fallback_dirs(
        override_dirs: Vec<PathBuf>,
        fallback_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            override_dirs,
            fallback_dirs,
        }
    }

    fn get_default_override_dirs() -> Vec<PathBuf> {
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

        debug!("Initialized override completion directories: {:?}", dirs);
        dirs
    }

    fn get_default_fallback_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let local_dir = PathBuf::from("./completions");
        debug!("Adding local fallback completion dir: {:?}", local_dir);
        dirs.push(local_dir);

        debug!("Initialized fallback completion directories: {:?}", dirs);
        dirs
    }

    pub fn load_database(&self) -> Result<Arc<CommandCompletionDatabase>> {
        // Keep the eager database to embedded resources. Filesystem definitions are loaded
        // lazily by load_command_completion so user overrides do not change this cache.
        match COMPLETION_DATABASE_CACHE.get() {
            Some(database) => {
                debug!("Using cached completion database (already loaded)");
                Ok(Arc::clone(database))
            }
            None => {
                debug!(
                    "Starting completion database loading from embedded resources (first time)..."
                );
                let mut database = CommandCompletionDatabase::new();
                debug!("Loading completions from embedded resources...");
                let loaded_count = match self.load_from_embedded(&mut database) {
                    Ok(count) => {
                        debug!(
                            "Successfully loaded {} completion files from embedded resources",
                            count
                        );
                        count
                    }
                    Err(e) => {
                        warn!("Failed to load completions from embedded resources: {}", e);
                        return Err(e);
                    }
                };

                debug!(
                    "Completion database loading complete: {} embedded files loaded",
                    loaded_count
                );

                let shared_db = Arc::new(database);
                let _ = COMPLETION_DATABASE_CACHE.set(Arc::clone(&shared_db));

                Ok(shared_db)
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

        let mut value: Value = match serde_json::from_str(content_str) {
            Ok(value) => value,
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
        normalize_legacy_top_level_options(&mut value);

        let completion: CommandCompletion = match serde_json::from_value(value) {
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

        // Validate arguments
        for argument in &completion.arguments {
            self.validate_argument(argument, &completion.command)?;
        }

        Ok(())
    }

    /// Validate argument
    fn validate_argument(&self, argument: &super::command::Argument, context: &str) -> Result<()> {
        if argument.name.is_empty() {
            anyhow::bail!("Argument name cannot be empty in '{}'", context);
        }
        self.validate_argument_type(argument.arg_type.as_ref(), context);
        Ok(())
    }

    fn validate_argument_type(&self, arg_type: Option<&ArgumentType>, context: &str) {
        if let Some(ArgumentType::Dynamic { provider, .. }) = arg_type
            && !super::dynamic::is_known_declared_dynamic_provider(provider)
        {
            warn!(
                "Unknown dynamic completion provider '{}' in '{}'",
                provider, context
            );
        }
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

        // Validate arguments
        for argument in &subcommand.arguments {
            self.validate_argument(argument, &format!("{} {}", parent_command, subcommand.name))?;
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

        if let Some(ref short) = option.short
            && !valid_short_option(short)
        {
            anyhow::bail!("Invalid short option format '{}' in '{}'", short, context);
        }

        if let Some(ref long) = option.long
            && !valid_long_option(long)
        {
            anyhow::bail!("Invalid long option format '{}' in '{}'", long, context);
        }

        if let Some(argument) = &option.argument {
            self.validate_argument(argument, context)?;
        }
        self.validate_argument_type(option.value_type.as_ref(), context);

        Ok(())
    }

    /// Load completion data for specific command.
    ///
    /// Filesystem directories are intentionally checked before embedded assets:
    /// `comp-gen` writes to the user config directory and must be able to
    /// override an existing built-in definition such as `git.json`.
    pub fn load_command_completion(&self, command_name: &str) -> Result<Option<CommandCompletion>> {
        let filename = format!("{command_name}.json");

        debug!("Checking override filesystem directories for: {}", filename);
        if let Some(completion) =
            self.load_command_completion_from_dirs(command_name, &filename, &self.override_dirs)?
        {
            return Ok(Some(completion));
        }

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

        debug!("Checking fallback filesystem directories for: {}", filename);
        if let Some(completion) =
            self.load_command_completion_from_dirs(command_name, &filename, &self.fallback_dirs)?
        {
            return Ok(Some(completion));
        }

        debug!("No completion found for command: {}", command_name);
        Ok(None)
    }

    fn load_command_completion_from_dirs(
        &self,
        command_name: &str,
        filename: &str,
        dirs: &[PathBuf],
    ) -> Result<Option<CommandCompletion>> {
        for dir in dirs {
            let path = dir.join(filename);
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
        debug!("Collecting completions from override filesystem directories...");
        for dir in self.override_dirs.iter().chain(self.fallback_dirs.iter()) {
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

fn normalize_legacy_top_level_options(value: &mut Value) {
    let Value::Object(object) = value else {
        return;
    };

    let Some(options) = object.remove("options") else {
        return;
    };

    let options = match options {
        Value::Array(options) => options,
        other => {
            object.insert("options".to_string(), other);
            return;
        }
    };

    match object.get_mut("global_options") {
        Some(Value::Array(global_options)) => global_options.extend(options),
        Some(_) => {
            object.insert("options".to_string(), Value::Array(options));
        }
        None => {
            object.insert("global_options".to_string(), Value::Array(options));
        }
    }
}

fn option_base(option: &str) -> &str {
    option.split_whitespace().next().unwrap_or("")
}

fn valid_short_option(option: &str) -> bool {
    let base = option_base(option);
    base.starts_with('-') && !base.starts_with("--") && base.len() > 1
}

fn valid_long_option(option: &str) -> bool {
    let base = option_base(option);
    (base.starts_with('-') || base.starts_with('+')) && base.len() > 1 && base != "--"
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
    fn test_load_option_value_type_fields() {
        let loader = JsonCompletionLoader::new();
        let completion = loader
            .load_completion_from_content(
                br#"
                {
                    "command": "kubectl",
                    "global_options": [
                        {
                            "short": "-n",
                            "long": "--namespace",
                            "description": "Namespace",
                            "takes_value": true,
                            "value_type": {
                                "type": "Choice",
                                "data": ["default", "kube-system"]
                            }
                        }
                    ]
                }
                "#,
                "inline",
            )
            .unwrap();

        let option = &completion.global_options[0];
        assert!(option.takes_value);
        assert!(matches!(
            option.value_type(),
            Some(crate::completion::command::ArgumentType::Choice(values))
                if values == &vec!["default".to_string(), "kube-system".to_string()]
        ));
    }

    #[test]
    fn filesystem_completion_overrides_embedded_completion() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("git.json"),
            r#"
            {
                "command": "git",
                "description": "User override",
                "subcommands": [
                    {
                        "name": "custom-user-subcommand",
                        "description": "Only in user config"
                    }
                ]
            }
            "#,
        )
        .unwrap();

        let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
        let completion = loader.load_command_completion("git").unwrap().unwrap();

        assert_eq!(completion.description.as_deref(), Some("User override"));
        assert!(
            completion
                .subcommands
                .iter()
                .any(|subcommand| subcommand.name == "custom-user-subcommand")
        );
    }

    #[test]
    fn fallback_completion_does_not_override_embedded_completion() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("git.json"),
            r#"
            {
                "command": "git",
                "description": "Fallback override",
                "subcommands": [
                    {
                        "name": "fallback-only-subcommand",
                        "description": "Only in fallback"
                    }
                ]
            }
            "#,
        )
        .unwrap();

        let loader = JsonCompletionLoader::with_override_and_fallback_dirs(
            Vec::new(),
            vec![temp_dir.path().to_path_buf()],
        );
        let completion = loader.load_command_completion("git").unwrap().unwrap();

        assert_ne!(completion.description.as_deref(), Some("Fallback override"));
        assert!(
            !completion
                .subcommands
                .iter()
                .any(|subcommand| subcommand.name == "fallback-only-subcommand")
        );
    }

    #[test]
    fn fallback_completion_loads_when_embedded_missing() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("local-only-command.json"),
            r#"
            {
                "command": "local-only-command",
                "description": "Fallback-only completion"
            }
            "#,
        )
        .unwrap();

        let loader = JsonCompletionLoader::with_override_and_fallback_dirs(
            Vec::new(),
            vec![temp_dir.path().to_path_buf()],
        );
        let completion = loader
            .load_command_completion("local-only-command")
            .unwrap()
            .unwrap();

        assert_eq!(
            completion.description.as_deref(),
            Some("Fallback-only completion")
        );
    }

    #[test]
    fn command_completion_schema_matches_runtime_field_names() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../command-completion-schema.json")).unwrap();
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(properties.contains_key("arguments"));

        let argument_properties = schema
            .pointer("/definitions/Argument/properties")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(argument_properties.contains_key("type"));
        assert!(!argument_properties.contains_key("arg_type"));

        let option_properties = schema
            .pointer("/definitions/CommandOption/properties")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(option_properties.contains_key("argument"));

        for type_name in [
            "Process",
            "CommandWithArgs",
            "User",
            "Group",
            "Signal",
            "Interface",
            "Dynamic",
        ] {
            assert!(
                schema.to_string().contains(&format!(r#""{type_name}""#)),
                "schema should include runtime ArgumentType::{type_name}"
            );
        }
    }

    #[test]
    fn command_completion_schema_uses_shared_dynamic_provider_list() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../command-completion-schema.json")).unwrap();
        let dynamic_type = schema
            .pointer("/definitions/ArgumentType/oneOf")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .find(|entry| {
                entry.get("title").and_then(serde_json::Value::as_str) == Some("Dynamic Type")
            })
            .unwrap();
        let schema_providers = dynamic_type
            .pointer("/properties/data/properties/provider/enum")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            schema_providers,
            dsh_types::completion::DYNAMIC_COMPLETION_PROVIDERS
        );
    }

    #[test]
    fn top_level_options_are_merged_with_global_options() {
        let loader = JsonCompletionLoader::new();
        let completion = loader
            .load_completion_from_content(
                br#"{
                    "command": "legacy-options",
                    "global_options": [
                        { "long": "--help", "description": "Show help" }
                    ],
                    "options": [
                        { "long": "--verbose", "short": "-v", "description": "Verbose output" }
                    ]
                }"#,
                "legacy-options.json",
            )
            .unwrap();

        assert!(
            completion
                .global_options
                .iter()
                .any(|option| option.long.as_deref() == Some("--help"))
        );
        assert!(
            completion
                .global_options
                .iter()
                .any(|option| option.long.as_deref() == Some("--verbose"))
        );
    }

    #[test]
    fn root_completion_mirror_matches_embedded_completion_source() {
        let embedded_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("completions");
        let mirror_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../completions");
        if !mirror_dir.exists() {
            return;
        }

        let mut embedded_files = Vec::new();
        for entry in fs::read_dir(&embedded_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                embedded_files.push(path);
            }
        }
        embedded_files.sort();

        for embedded_path in embedded_files {
            let file_name = embedded_path.file_name().unwrap();
            let mirror_path = mirror_dir.join(file_name);
            assert!(
                mirror_path.exists(),
                "root completion mirror is missing {}",
                file_name.to_string_lossy()
            );
            assert_eq!(
                fs::read(&embedded_path).unwrap(),
                fs::read(&mirror_path).unwrap(),
                "root completion mirror differs for {}",
                file_name.to_string_lossy()
            );
        }
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
            arguments: vec![],
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

        // Should include both embedded completions and filesystem completions.
        // The exact number grows as built-in command definitions are added.
        assert!(completions.len() >= 5);
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
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&option_with_short, "test").is_ok());

        // Test long option with placeholder
        let option_with_long = crate::completion::command::CommandOption {
            short: None,
            long: Some("--type <TYPE>".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&option_with_long, "test").is_ok());

        // Test both short and long with placeholders
        let option_both = crate::completion::command::CommandOption {
            short: Some("-f <FILE>".to_string()),
            long: Some("--file <FILE>".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&option_both, "test").is_ok());

        // Test invalid short options (short must start with a single dash and have content)
        let invalid_short = crate::completion::command::CommandOption {
            short: Some("--".to_string()), // Invalid: this is a long option prefix, not a short option
            long: None,
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&invalid_short, "test").is_err());

        let invalid_bare_short = crate::completion::command::CommandOption {
            short: Some("-".to_string()),
            long: None,
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&invalid_bare_short, "test").is_err());

        // Test that valid short option like -123 is now accepted
        let valid_short_with_number = crate::completion::command::CommandOption {
            short: Some("-123".to_string()), // Should be valid now: starts with -
            long: None,
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(
            loader
                .validate_option(&valid_short_with_number, "test")
                .is_ok()
        );

        let valid_short_with_attached_value = crate::completion::command::CommandOption {
            short: Some("-ofile".to_string()),
            long: None,
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(
            loader
                .validate_option(&valid_short_with_attached_value, "test")
                .is_ok()
        );

        // Test invalid long option (should still fail)
        let invalid_long = crate::completion::command::CommandOption {
            short: None,
            long: Some("--".to_string()), // Invalid: just -- without any content
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&invalid_long, "test").is_err());

        let invalid_long_with_placeholder = crate::completion::command::CommandOption {
            short: None,
            long: Some("-- <ARG>".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(
            loader
                .validate_option(&invalid_long_with_placeholder, "test")
                .is_err()
        );

        let invalid_bare_long = crate::completion::command::CommandOption {
            short: None,
            long: Some("-".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(loader.validate_option(&invalid_bare_long, "test").is_err());

        // Test that long option starting with -- and containing numbers is now valid
        let valid_long_with_number = crate::completion::command::CommandOption {
            short: None,
            long: Some("--123invalid".to_string()), // Should be valid now: starts with --
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(
            loader
                .validate_option(&valid_long_with_number, "test")
                .is_ok()
        );

        let valid_single_dash_long = crate::completion::command::CommandOption {
            short: None,
            long: Some("-Xmx".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        };
        assert!(
            loader
                .validate_option(&valid_single_dash_long, "test")
                .is_ok()
        );
    }

    /// Test that all embedded JSON completion files can be loaded and parsed correctly
    #[test]
    fn test_all_embedded_completion_files_load_correctly() {
        let loader = JsonCompletionLoader::new();
        // Directly iterate over embedded assets, do not use list_available_completions
        // This ensures the test is hermetic and only checks what's compiled in.
        let embedded_commands: Vec<String> = CompletionAssets::iter()
            .filter(|path| path.ends_with(".json"))
            .map(|path| path.strip_suffix(".json").unwrap().to_string())
            .collect();

        for command_name in embedded_commands {
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
        let generator = CompletionGenerator::new(&database);

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
                    raw_args: vec![],
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
                            raw_args: vec![],
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
        let generator = CompletionGenerator::new(&database);

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
                    raw_args: vec![],
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

    #[test]
    fn test_git_completion_loading() {
        use crate::completion::command::ArgumentType;

        // Initialize loader
        let loader = JsonCompletionLoader::new();
        let mut database = CommandCompletionDatabase::new();

        // Load database from embedded
        let result = loader.load_from_embedded(&mut database);
        assert!(
            result.is_ok(),
            "Failed to load from embedded resources: {:?}",
            result.err()
        );

        // Verify git command exists
        let git_cmd = database.get_command("git");
        assert!(git_cmd.is_some(), "git command not found in database");
        let git_cmd = git_cmd.unwrap();

        // Verify switch subcommand
        let switch_sub = git_cmd.subcommands.iter().find(|s| s.name == "switch");
        assert!(switch_sub.is_some(), "git switch subcommand not found");
        let switch_sub = switch_sub.unwrap();

        // Verify argument type is Dynamic
        assert!(
            !switch_sub.arguments.is_empty(),
            "git switch has no arguments"
        );
        let branch_arg = &switch_sub.arguments[0];

        match &branch_arg.arg_type {
            Some(ArgumentType::Dynamic { provider, scope }) => {
                assert_eq!(provider, "git.branch");
                assert_eq!(scope.as_deref(), Some("project"));
            }
            _ => panic!(
                "Expected Dynamic argument type for git switch, found {:?}",
                branch_arg.arg_type
            ),
        }
    }

    #[test]
    fn embedded_completion_definitions_do_not_use_script_type() {
        let embedded_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("completions");
        for entry in fs::read_dir(&embedded_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let contents = fs::read_to_string(&path).unwrap();
            let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
            assert!(
                !json_contains_script_type(&value),
                "built-in completion must not use Script: {}",
                path.display()
            );
        }
    }

    fn json_contains_script_type(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(object) => {
                object.get("type").and_then(serde_json::Value::as_str) == Some("Script")
                    || object.values().any(json_contains_script_type)
            }
            serde_json::Value::Array(values) => values.iter().any(json_contains_script_type),
            _ => false,
        }
    }
}
