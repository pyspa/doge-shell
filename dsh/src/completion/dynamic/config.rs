use serde::Deserialize;

/// Configuration for a dynamic completion handler
///
/// This structure represents the overall configuration for dynamic completions.
/// Each configuration file contains a list of dynamic completion definitions
/// that specify how to provide dynamic completion candidates for various commands.
///
/// The configuration is loaded at startup and embedded into the binary for distribution,
/// while also allowing user-defined configurations to override or extend the defaults.
///
/// Example configuration file structure:
/// ```toml
/// [[dynamic_completions]]
/// command = "git"
/// subcommands = ["checkout", "switch"]
/// description = "Complete git branch names"
/// match_condition = { type = "HasSubcommand" }
/// shell_command = "git branch --format='%(refname:short)' | grep -v '^origin/'"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct DynamicCompletionConfig {
    /// List of dynamic completion definitions
    /// Each definition specifies how to provide completions for a specific command pattern
    pub dynamic_completions: Vec<DynamicCompletionDef>,
}

/// Definition for a single dynamic completion
///
/// This structure defines the parameters for a single dynamic completion handler.
/// It specifies which commands and contexts trigger the completion, what command
/// to execute to generate candidates, and how to process the results.
#[derive(Debug, Clone, Deserialize)]
pub struct DynamicCompletionDef {
    /// The main command to match (e.g., "git", "sudo")
    /// This is the primary command that triggers this completion handler
    pub command: String,

    /// Optional list of subcommands to match
    /// When empty, this completion applies to the main command only
    /// When specified, the completion applies only when these subcommands are present
    #[serde(default)]
    pub subcommands: Vec<String>,

    /// Description of what this completion does
    /// Used to provide context information about the completion candidates
    pub description: String,

    /// Condition for when to match this completion handler
    /// Specifies the context in which this completion should be active
    pub match_condition: MatchCondition,

    /// Shell command to execute to generate candidates
    /// The output of this command will be processed to create completion candidates
    pub shell_command: String,

    /// Optional filter to apply to output
    /// If specified, only candidates containing this string will be included
    #[serde(default)]
    pub filter_output: Option<String>,

    /// Optional priority (higher = shown first)
    /// Used to order completion candidates in the UI
    #[serde(default = "default_priority")]
    pub priority: u32,
}

/// Condition for when to apply the completion
///
/// This enum defines different strategies for determining when a dynamic completion
/// handler should be active. Each variant represents a different matching strategy.
///
/// The configuration uses a tagged enum format where each variant has a "type" field
/// and an optional "params" field containing the specific parameters for that type.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "params")]
#[derive(Default)]
pub enum MatchCondition {
    /// Match when command matches (e.g., "kill")
    /// Triggers when the main command matches, regardless of subcommands or arguments
    #[default]
    StartsWithCommand,

    /// Match when command and specific subcommands match (e.g., "git checkout")
    /// Triggers only when both the main command and one of the specified subcommands are present
    HasSubcommand,

    /// Match when command, subcommand, and option match (e.g., "sudo pacman -S")
    /// Triggers for complex command patterns involving options
    HasSubcommandAndOption,

    /// Match when second argument is being completed
    /// Triggers when the user is typing the second argument of a command
    SecondArgument,

    /// Match when third argument is being completed
    /// Triggers when the user is typing the third argument of a command
    ThirdArgument,

    /// Custom match pattern with more complex conditions
    /// Allows for flexible matching using the CustomMatchPattern structure
    CustomPattern(CustomMatchPattern),
}

/// Custom matching pattern with flexible conditions
///
/// This structure provides a flexible way to define complex matching conditions
/// that combine multiple criteria. It allows for fine-grained control over when
/// a completion handler should be active.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomMatchPattern {
    /// Command must match this
    /// If specified, the main command must match this value
    pub command: Option<String>,

    /// Subcommands to match (if any)
    /// If specified and non-empty, one of these subcommands must be present
    #[serde(default)]
    pub subcommands: Option<Vec<String>>,

    /// Args that must contain these values
    /// If specified, the arguments must contain these values as substrings
    #[serde(default)]
    pub args_contains: Option<Vec<String>>,

    /// Options that must be present
    /// If specified, the command line must include these options
    #[serde(default)]
    pub options_contains: Option<Vec<String>>,

    /// If true, command must have no arguments
    /// This is useful for matching commands when no arguments have been specified yet
    #[serde(default)]
    pub args_must_be_empty: Option<bool>,

    /// Specific positional arguments that must match at specific indices
    /// Format: vector of (index, expected_value) pairs
    /// Example: vec![(0, "pacman".to_string()), (1, "-S".to_string())] means
    /// args[0] must be "pacman" and args[1] must contain "-S"
    #[serde(default)]
    pub args_positional: Option<Vec<(usize, String)>>,
}

fn default_priority() -> u32 {
    100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_git_config() {
        let toml_str = r#"
            [[dynamic_completions]]
            command = "git"
            subcommands = ["checkout", "switch"]
            description = "Complete git branches"
            match_condition = { type = "HasSubcommand" }
            shell_command = "git branch --format='%(refname:short)'"
        "#;

        let config: DynamicCompletionConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.dynamic_completions.len(), 1);
        assert_eq!(config.dynamic_completions[0].command, "git");
        assert_eq!(
            config.dynamic_completions[0].subcommands,
            vec!["checkout", "switch"]
        );
    }

    #[test]
    fn test_deserialize_with_custom_pattern() {
        let toml_str = r#"
            [[dynamic_completions]]
            command = "sudo"
            description = "Complete with available pacman packages"
            match_condition = { type = "CustomPattern", params = { command = "sudo", args_contains = ["pacman", "S"] } }
            shell_command = "pacman -Slq"
        "#;

        let config: DynamicCompletionConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.dynamic_completions.len(), 1);
        match &config.dynamic_completions[0].match_condition {
            MatchCondition::CustomPattern(pattern) => {
                assert_eq!(pattern.command, Some("sudo".to_string()));
                assert_eq!(
                    pattern.args_contains,
                    Some(vec!["pacman".to_string(), "S".to_string()])
                );
            }
            _ => panic!("Expected CustomPattern match condition"),
        }
    }

    #[test]
    fn test_deserialize_multiple_configurations() {
        let toml_str = r#"
            [[dynamic_completions]]
            command = "git"
            subcommands = ["checkout"]
            description = "Complete git branches"
            match_condition = { type = "HasSubcommand" }
            shell_command = "git branch --format='%(refname:short)'"

            [[dynamic_completions]]
            command = "kill"
            description = "Complete with running process IDs"
            match_condition = { type = "StartsWithCommand" }
            shell_command = "ps -xo pid,comm --no-headers"
        "#;

        let config: DynamicCompletionConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.dynamic_completions.len(), 2);
        assert_eq!(config.dynamic_completions[0].command, "git");
        assert_eq!(config.dynamic_completions[1].command, "kill");
    }

    #[test]
    fn test_deserialize_with_filter_and_priority() {
        let toml_str = r#"
            [[dynamic_completions]]
            command = "ls"
            description = "Complete with files in current directory"
            match_condition = { type = "StartsWithCommand" }
            shell_command = "ls"
            filter_output = "test"
            priority = 200
        "#;

        let config: DynamicCompletionConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.dynamic_completions.len(), 1);
        let entry = &config.dynamic_completions[0];
        assert_eq!(entry.command, "ls");
        assert_eq!(entry.filter_output, Some("test".to_string()));
        assert_eq!(entry.priority, 200);
    }

    #[test]
    fn test_deserialize_with_default_priority() {
        let toml_str = r#"
            [[dynamic_completions]]
            command = "test"
            description = "Test command"
            match_condition = { type = "StartsWithCommand" }
            shell_command = "echo test"
        "#;

        let config: DynamicCompletionConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.dynamic_completions[0].priority, 100); // Default priority
    }
}
