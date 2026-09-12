use super::command::{ArgumentType, CommandCompletion, CommandCompletionDatabase};
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
/// The repository-root `completions/` directory is the single canonical source
/// and is embedded from here. User config completion directories are checked
/// before embedded assets so generated definitions can override built-ins;
/// local dev fallback directories are checked after embedded.
#[derive(RustEmbed)]
#[folder = "../completions/"]
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
        let dirs = crate::environment::user_asset_override_dirs("completions");
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
mod tests;
