//! Command, subcommand and option definitions loaded from completion JSON.
//! The lookup helpers resolve aliases and option visibility along a command path.
//! The database holds these definitions for completion and inline explanations.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Overall structure for command completion information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandCompletion {
    /// Command name (e.g., "git", "cargo", "docker")
    pub command: String,
    /// Offer additional short flags on the same word when this CLI accepts clusters.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_option_clusters: Option<ShortOptionClusters>,
    /// Command description
    pub description: Option<String>,
    /// List of subcommands
    #[serde(default)]
    pub subcommands: Vec<SubCommand>,
    /// Global options (common to all subcommands)
    #[serde(default)]
    pub global_options: Vec<CommandOption>,
    /// Arguments that this command accepts (for top-level commands)
    #[serde(default)]
    pub arguments: Vec<Argument>,
}

impl CommandCompletion {
    /// Top-level definitions visible at this point in the command line.
    pub(crate) fn visible_top_level_options(
        &self,
        path: &[String],
    ) -> impl Iterator<Item = &CommandOption> {
        let at_root = path.is_empty();
        self.global_options
            .iter()
            .filter(move |opt| at_root || opt.inherit != Some(false))
    }

    pub(crate) fn subcommands_on_path<'db>(
        &'db self,
        path: &[String],
    ) -> impl Iterator<Item = &'db SubCommand> {
        let mut children = &self.subcommands;
        path.iter().map_while(move |name| {
            let sub = children.iter().find(|s| s.matches(name))?;
            children = &sub.subcommands;
            Some(sub)
        })
    }

    pub(crate) fn arguments_for_path(&self, path: &[String]) -> &[Argument] {
        self.subcommands_on_path(path)
            .last()
            .map_or(self.arguments.as_slice(), |sub| sub.arguments.as_slice())
    }

    pub(super) fn has_scoped_options_on_path(&self, path: &[String]) -> bool {
        self.global_options.iter().any(|opt| opt.inherit.is_some())
            || self
                .subcommands_on_path(path)
                .any(|sub| sub.options.iter().any(|opt| opt.inherit.is_some()))
    }

    /// Options used to interpret argv, including inherited options along the path.
    pub(crate) fn options_for_path(&self, path: &[String]) -> Vec<&CommandOption> {
        self.collect_options(path, true)
    }

    pub(super) fn option_candidates_for_path(&self, path: &[String]) -> Vec<&CommandOption> {
        self.collect_options(path, false)
    }

    fn collect_options(
        &self,
        path: &[String],
        include_legacy_parents: bool,
    ) -> Vec<&CommandOption> {
        let mut options: Vec<_> = self.visible_top_level_options(path).collect();
        for (depth, sub) in self.subcommands_on_path(path).enumerate() {
            options.extend(sub.options.iter().filter(|opt| {
                depth + 1 == path.len() || opt.inherit.unwrap_or(include_legacy_parents)
            }));
        }
        options
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShortOptionClusters {
    All(bool),
    Letters(String),
}

impl ShortOptionClusters {
    pub(super) fn enabled(&self) -> bool {
        !matches!(self, Self::All(false))
    }

    pub(super) fn allows(&self, letter: char) -> bool {
        letter.is_ascii_alphanumeric()
            && match self {
                Self::All(all) => *all,
                Self::Letters(letters) => letters.contains(letter),
            }
    }
}

/// Subcommand definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubCommand {
    /// Subcommand name (e.g., "add", "commit", "push")
    pub name: String,
    /// Subcommand description
    pub description: Option<String>,
    /// Subcommand aliases
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Options specific to this subcommand
    #[serde(default)]
    pub options: Vec<CommandOption>,
    /// Arguments that this subcommand accepts
    #[serde(default)]
    pub arguments: Vec<Argument>,
    /// Nested subcommands (e.g., git remote add)
    #[serde(default)]
    pub subcommands: Vec<SubCommand>,
}

impl SubCommand {
    pub(super) fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|alias| alias == name)
    }
}

/// Option definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOption {
    /// Short option (e.g., "-v")
    pub short: Option<String>,
    /// Long option (e.g., "--verbose")
    pub long: Option<String>,
    /// Option description
    pub description: Option<String>,
    /// Explicit visibility in descendants; omission preserves legacy completion definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit: Option<bool>,
    /// Whether this option expects a following value.
    #[serde(default)]
    pub takes_value: bool,
    /// An optional value belongs to the option word and never consumes the next argument.
    #[serde(default)]
    pub value_attached_only: bool,
    /// Whether this option may be specified more than once (JSON `multiple`).
    #[serde(default)]
    pub multiple: bool,
    /// Option value type used by JSON completion definitions.
    #[serde(default)]
    pub value_type: Option<ArgumentType>,
    /// Option argument definition (if any)
    #[serde(default)]
    pub argument: Option<Argument>,
}

impl CommandOption {
    pub fn matches_name(&self, name: &str) -> bool {
        self.short.as_deref() == Some(name) || self.long.as_deref() == Some(name)
    }

    pub fn value_type(&self) -> Option<&ArgumentType> {
        self.argument
            .as_ref()
            .and_then(|argument| argument.arg_type.as_ref())
            .or(self.value_type.as_ref())
    }

    pub fn expects_value(&self) -> bool {
        self.argument.is_some() || self.value_type.is_some() || self.takes_value
    }

    pub(super) fn takes_separate_value(&self) -> bool {
        self.expects_value() && !self.value_attached_only
    }
}

/// Argument definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Argument {
    /// Argument name
    pub name: String,
    /// Argument description
    pub description: Option<String>,
    /// Whether the final argument definition can repeat
    #[serde(default)]
    pub multiple: bool,
    /// Argument type
    #[serde(default, rename = "type")]
    pub arg_type: Option<ArgumentType>,
}

/// Argument/value types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum ArgumentType {
    /// File path
    File {
        /// File extension filter (e.g., [".rs", ".toml"])
        extensions: Option<Vec<String>>,
    },
    /// Directory path
    Directory,
    /// Arbitrary string
    String,
    /// Number
    Number,
    /// Choice from options
    Choice(Vec<String>),
    /// Existing command name
    Command,
    /// Environment variable name
    Environment,
    /// URL
    Url,
    /// Regular expression pattern
    /// Regular expression pattern
    Regex,
    /// Script to execute for dynamic completion
    Script(String),
    /// Cached dynamic provider declared by JSON completion definitions
    Dynamic {
        /// Stable provider id (for example, "git.branch")
        provider: String,
        /// Optional provider scope hint
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    /// System process (PID)
    Process,
    /// Command with arguments (delegates completion to the command)
    CommandWithArgs,
    /// System user name (from /etc/passwd)
    User,
    /// System group name (from /etc/group)
    Group,
    /// Signal name (SIGTERM, SIGKILL, etc.)
    Signal,
    /// Network interface name
    Interface,
}

/// Command completion database
#[derive(Debug, Clone)]
pub struct CommandCompletionDatabase {
    /// Map of command name -> completion information
    commands: HashMap<String, CommandCompletion>,
}

impl CommandCompletionDatabase {
    /// Create a new empty database
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// Add command completion information
    pub fn add_command(&mut self, completion: CommandCompletion) {
        self.commands.insert(completion.command.clone(), completion);
    }

    /// Get completion information from command name
    pub fn get_command(&self, command: &str) -> Option<&CommandCompletion> {
        self.commands.get(command)
    }

    /// Get list of registered command names
    pub fn get_command_names(&self) -> Vec<&String> {
        self.commands.keys().collect()
    }

    /// Iterate over registered command completions
    pub fn iter_commands(&self) -> impl Iterator<Item = (&String, &CommandCompletion)> {
        self.commands.iter()
    }

    /// Number of registered commands
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Check if database is empty
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for CommandCompletionDatabase {
    fn default() -> Self {
        Self::new()
    }
}

/// Completion candidate type
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionType {
    /// Subcommand
    SubCommand,
    /// Option (short form)
    ShortOption,
    /// Option (long form)
    LongOption,
    /// Argument value
    Argument,
    /// File
    File,
    /// Directory
    Directory,
    /// Filesystem paths generated from an unquoted `~/` source prefix.
    HomeFile,
    HomeDirectory,
    /// Process
    Process,
}

/// Completion candidate
#[derive(Debug, Clone)]
pub struct CompletionCandidate {
    /// Candidate string
    pub text: String,
    /// Description
    pub description: Option<String>,
    /// Completion type
    pub completion_type: CompletionType,
    /// Priority (higher values displayed first)
    pub priority: u32,
}

impl CompletionCandidate {
    /// Create a new completion candidate
    pub fn new(
        text: String,
        description: Option<String>,
        completion_type: CompletionType,
        priority: u32,
    ) -> Self {
        Self {
            text,
            description,
            completion_type,
            priority,
        }
    }

    /// Create subcommand candidate
    pub fn subcommand(name: String, description: Option<String>) -> Self {
        Self::new(name, description, CompletionType::SubCommand, 100)
    }

    /// Create short option candidate
    pub fn short_option(option: String, description: Option<String>) -> Self {
        Self::new(option, description, CompletionType::ShortOption, 80)
    }

    /// Create long option candidate
    pub fn long_option(option: String, description: Option<String>) -> Self {
        Self::new(option, description, CompletionType::LongOption, 80)
    }

    /// Create argument candidate
    pub fn argument(value: String, description: Option<String>) -> Self {
        Self::new(value, description, CompletionType::Argument, 60)
    }

    /// Create file candidate
    pub fn file(path: String) -> Self {
        Self::new(path, None, CompletionType::File, 40)
    }

    /// Create directory candidate
    pub fn directory(path: String) -> Self {
        Self::new(path, None, CompletionType::Directory, 50)
    }

    /// Create process candidate
    pub fn process(pid: String, description: Option<String>) -> Self {
        Self::new(pid, description, CompletionType::Process, 70)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_completion_database() {
        let mut db = CommandCompletionDatabase::new();
        assert!(db.commands.is_empty());
        assert_eq!(db.len(), 0);

        let git_completion = CommandCompletion {
            short_option_clusters: None,
            command: "git".to_string(),
            description: Some("Git version control system".to_string()),
            subcommands: vec![],
            global_options: vec![],
            arguments: vec![],
        };

        db.add_command(git_completion);
        assert!(!db.commands.is_empty());
        assert_eq!(db.len(), 1);
        assert!(db.get_command("git").is_some());
        assert!(db.get_command("nonexistent").is_none());
    }

    #[test]
    fn test_completion_candidate_creation() {
        let subcommand = CompletionCandidate::subcommand(
            "add".to_string(),
            Some("Add files to index".to_string()),
        );
        assert_eq!(subcommand.completion_type, CompletionType::SubCommand);
        assert_eq!(subcommand.priority, 100);

        let option = CompletionCandidate::long_option(
            "--verbose".to_string(),
            Some("Verbose output".to_string()),
        );
        assert_eq!(option.completion_type, CompletionType::LongOption);
        assert_eq!(option.priority, 80);
    }

    #[test]
    fn test_argument_type_serialization() {
        let file_type = ArgumentType::File {
            extensions: Some(vec![".rs".to_string(), ".toml".to_string()]),
        };
        let json = serde_json::to_string(&file_type).unwrap();
        let deserialized: ArgumentType = serde_json::from_str(&json).unwrap();

        match deserialized {
            ArgumentType::File { extensions } => {
                assert_eq!(
                    extensions,
                    Some(vec![".rs".to_string(), ".toml".to_string()])
                );
            }
            _ => panic!("Deserialization failed"),
        }

        let dynamic_type = ArgumentType::Dynamic {
            provider: "git.branch".to_string(),
            scope: Some("project".to_string()),
        };
        let json = serde_json::to_string(&dynamic_type).unwrap();
        let deserialized: ArgumentType = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, dynamic_type);
    }
}
