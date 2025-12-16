use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Overall structure for command completion information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandCompletion {
    /// Command name (e.g., "git", "cargo", "docker")
    pub command: String,
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

/// Subcommand definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubCommand {
    /// Subcommand name (e.g., "add", "commit", "push")
    pub name: String,
    /// Subcommand description
    pub description: Option<String>,
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

/// Option definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOption {
    /// Short option (e.g., "-v")
    pub short: Option<String>,
    /// Long option (e.g., "--verbose")
    pub long: Option<String>,
    /// Option description
    pub description: Option<String>,
}

/// Argument definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Argument {
    /// Argument name
    pub name: String,
    /// Argument description
    pub description: Option<String>,
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
    /// System process (PID)
    Process,
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

    /// Number of registered commands
    pub fn len(&self) -> usize {
        self.commands.len()
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
    }
}
