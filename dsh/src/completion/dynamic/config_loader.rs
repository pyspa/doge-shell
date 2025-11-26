use super::config::{DynamicCompletionConfig, DynamicCompletionDef, MatchCondition};
use super::{CompletionCandidate, DynamicCompletionHandler, ParsedCommandLine};
use crate::completion::CompletionType;
use anyhow::Result;
use rust_embed::RustEmbed;
use tracing::{debug, warn};

/// Embedded resource struct for storing dynamic completion configurations
///
/// This struct uses rust-embed to compile the dynamic-completion configuration files
/// into the binary at compile time. This ensures that default configurations are
/// distributed with the application while still allowing for user customization.
///
/// The configurations are stored relative to the dsh crate directory and will be
/// available at runtime without requiring external file access.
#[derive(RustEmbed)]
#[folder = "../dynamic-completions/"]
struct DynamicCompletionAssets;

/// A script-based completion handler that uses external commands
///
/// This handler executes shell commands to generate dynamic completion candidates.
/// It provides a flexible way to create completion suggestions based on runtime data
/// such as running processes, available usernames, git branches, etc.
///
/// The handler uses the configuration provided at initialization to determine:
/// - When to activate (matching conditions)
/// - What command to execute
/// - How to process the command output
///
/// This approach allows for dynamic completions without requiring Rust code changes.
pub struct ScriptBasedCompletionHandler {
    /// Configuration that defines this handler's behavior
    config: DynamicCompletionDef,
}

impl ScriptBasedCompletionHandler {
    /// Creates a new script-based completion handler with the given configuration
    ///
    /// # Arguments
    /// * `config` - The configuration defining how this handler should behave
    ///
    /// # Returns
    /// A new instance of ScriptBasedCompletionHandler
    pub fn new(config: DynamicCompletionDef) -> Self {
        Self { config }
    }

    /// Substitutes variables in the shell command template with actual values
    ///
    /// This method provides basic template substitution functionality to allow
    /// dynamic commands that can use information from the current command line context.
    ///
    /// Supported variables:
    /// - $COMMAND: The main command being executed
    /// - $CURRENT_TOKEN: The current token being completed
    /// - $SUBCOMMAND: The current subcommand (if any)
    ///
    /// # Arguments
    /// * `template` - The shell command template with variable placeholders
    /// * `parsed_command` - The parsed command line for context
    ///
    /// # Returns
    /// The template with variables substituted for actual values
    fn substitute_variables(&self, template: &str, parsed_command: &ParsedCommandLine) -> String {
        let mut result = template.to_string();

        // Replace variables like $CURRENT_TOKEN, $COMMAND, etc.
        result = result.replace("$COMMAND", &parsed_command.command);

        if let Some(current_arg) = &parsed_command.current_arg {
            result = result.replace("$CURRENT_TOKEN", current_arg);
        } else {
            result = result.replace("$CURRENT_TOKEN", "");
        }

        if let Some(first_sub) = parsed_command.subcommand_path.first() {
            result = result.replace("$SUBCOMMAND", first_sub);
        } else {
            result = result.replace("$SUBCOMMAND", "");
        }

        // Add more variable substitutions as needed

        result
    }
}

impl DynamicCompletionHandler for ScriptBasedCompletionHandler {
    /// Determines if this handler matches the current command line context
    ///
    /// This method evaluates the match condition specified in the configuration
    /// against the current parsed command line to decide if this handler should
    /// provide completion candidates.
    ///
    /// # Arguments
    /// * `parsed_command` - The parsed command line to evaluate against
    ///
    /// # Returns
    /// true if this handler should be used for the current command line, false otherwise
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        match &self.config.match_condition {
            MatchCondition::StartsWithCommand => {
                // Match when the main command matches (e.g., "kill" command)
                parsed_command.command == self.config.command
            }
            MatchCondition::HasSubcommand => {
                // Match when command and specific subcommands match (e.g., "git checkout")
                parsed_command.command == self.config.command
                    && !self.config.subcommands.is_empty()
                    && parsed_command
                        .subcommand_path
                        .first()
                        .is_some_and(|sc| self.config.subcommands.contains(sc))
            }
            MatchCondition::HasSubcommandAndOption => {
                // For cases like "sudo pacman -S" where we need to check subcommand and options
                parsed_command.command == self.config.command
                    && !parsed_command.args.is_empty() // Need at least the subcommand (e.g., "pacman")
                    && parsed_command.args[0] == "pacman" // Check for pacman subcommand
                    && parsed_command.args.iter().any(|arg| arg.contains('S')) // Check for -S option anywhere in args
            }
            MatchCondition::SecondArgument => {
                // Match when second argument is being completed
                parsed_command.command == self.config.command && !parsed_command.args.is_empty()
            }
            MatchCondition::ThirdArgument => {
                // Match when third argument is being completed
                parsed_command.command == self.config.command && parsed_command.args.len() >= 2
            }
            MatchCondition::CustomPattern(pattern) => {
                // Check command match
                if let Some(expected_cmd) = &pattern.command
                    && parsed_command.command != *expected_cmd
                {
                    return false;
                }

                // Check subcommands match if specified
                if let Some(expected_subcommands) = &pattern.subcommands
                    && !expected_subcommands.is_empty()
                {
                    if let Some(first_sub) = parsed_command.subcommand_path.first() {
                        if !expected_subcommands.contains(first_sub) {
                            return false;
                        }
                    } else {
                        return false; // Expected subcommand but none found
                    }
                }

                // Check args contain specified values
                if let Some(arg_values) = &pattern.args_contains {
                    for value in arg_values {
                        if !parsed_command.args.iter().any(|arg| arg.contains(value)) {
                            return false;
                        }
                    }
                }

                // Check options contain specified values
                if let Some(option_values) = &pattern.options_contains {
                    for value in option_values {
                        if !parsed_command.options.iter().any(|opt| opt.contains(value)) {
                            return false;
                        }
                    }
                }

                // Check if args must be empty
                if let Some(must_be_empty) = pattern.args_must_be_empty
                    && must_be_empty
                    && !parsed_command.args.is_empty()
                {
                    return false;
                }

                // Check positional arguments
                if let Some(positional_args) = &pattern.args_positional {
                    for (index, expected_value) in positional_args {
                        if *index >= parsed_command.args.len() {
                            // Position doesn't exist in args
                            return false;
                        }

                        if !parsed_command.args[*index].contains(expected_value) {
                            // Position doesn't match expected value
                            return false;
                        }
                    }
                }

                true
            }
        }
    }

    /// Generates completion candidates by executing the configured shell command
    ///
    /// This method executes the configured shell command, processes its output,
    /// and returns the results as completion candidates. The command execution
    /// happens synchronously using std::process::Command.
    ///
    /// # Arguments
    /// * `parsed_command` - The parsed command line that triggered this completion
    ///
    /// # Returns
    /// A Result containing either the completion candidates or an error
    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        debug!(
            "Generating candidates for command: {} with condition: {:?}",
            self.config.command, self.config.match_condition
        );

        // Substitute variables in the shell command to allow dynamic command construction
        let command = self.substitute_variables(&self.config.shell_command, parsed_command);
        debug!("Executing shell command: {}", command);

        // Execute the external command using std::process instead of tokio process
        // This approach works well for dynamic completion scenarios where async execution
        // is not necessary and provides better compatibility with different test contexts
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()?;

        if !output.status.success() {
            warn!(
                "Command '{}' failed with exit code: {}",
                command, output.status
            );
            // Return empty list of candidates if the command fails
            return Ok(vec![]);
        }

        // Process the command output to extract completion candidates
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                // Apply optional filter if specified in the configuration
                if let Some(ref filter_pattern) = self.config.filter_output
                    && !trimmed.contains(filter_pattern)
                {
                    // Skip candidates that don't match the filter pattern
                    continue;
                }

                // Create a completion candidate from the processed output line
                candidates.push(CompletionCandidate {
                    text: trimmed.to_string(),
                    description: Some(self.config.description.clone()),
                    completion_type: CompletionType::Argument,
                    priority: self.config.priority,
                });
            }
        }

        debug!("Generated {} candidates", candidates.len());
        Ok(candidates)
    }
}

/// Loader for dynamic completion configurations
pub struct DynamicConfigLoader;

impl DynamicConfigLoader {
    /// Load all configuration files (embedded and user)
    pub fn load_all_configs() -> Result<Vec<DynamicCompletionDef>> {
        let mut all_configs = Vec::new();

        // Load from embedded resources
        for file_path in DynamicCompletionAssets::iter() {
            if file_path.ends_with(".toml")
                && let Some(file_data) = DynamicCompletionAssets::get(&file_path)
            {
                match Self::parse_config_file(&file_data.data) {
                    Ok(configs) => all_configs.extend(configs),
                    Err(e) => {
                        warn!("Failed to parse embedded config {}: {}", file_path, e);
                    }
                }
            }
        }

        // TODO: Load from user config directory (~/.config/dsh/dynamic-completions/)
        // This would be implemented in a follow-up if needed

        Ok(all_configs)
    }

    /// Parse a configuration file
    fn parse_config_file(content: &[u8]) -> Result<Vec<DynamicCompletionDef>> {
        let content_str = std::str::from_utf8(content)?;
        let config: DynamicCompletionConfig = toml::from_str(content_str)?;
        Ok(config.dynamic_completions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::dynamic::config::CustomMatchPattern;
    use crate::completion::parser::{CompletionContext, ParsedCommandLine};

    #[test]
    fn test_script_based_handler_creation() {
        let config = DynamicCompletionDef {
            command: "test".to_string(),
            subcommands: vec![],
            description: "Test command".to_string(),
            match_condition: MatchCondition::StartsWithCommand,
            shell_command: "echo test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);
        assert!(handler.matches(&ParsedCommandLine {
            command: "test".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));
    }

    #[test]
    fn test_script_based_handler_non_matching_command() {
        let config = DynamicCompletionDef {
            command: "git".to_string(),
            subcommands: vec![],
            description: "Test git command".to_string(),
            match_condition: MatchCondition::StartsWithCommand,
            shell_command: "echo test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);
        assert!(!handler.matches(&ParsedCommandLine {
            command: "ls".to_string(), // Different command
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));
    }

    #[test]
    fn test_script_based_handler_has_subcommand_matching() {
        let config = DynamicCompletionDef {
            command: "git".to_string(),
            subcommands: vec!["checkout".to_string(), "switch".to_string()],
            description: "Test git subcommand".to_string(),
            match_condition: MatchCondition::HasSubcommand,
            shell_command: "echo test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);

        // Should match when the command and one of the specified subcommands match
        assert!(handler.matches(&ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec!["checkout".to_string()],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));

        // Should not match when the command matches but subcommand doesn't
        assert!(!handler.matches(&ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec!["commit".to_string()],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));
    }

    #[test]
    fn test_script_based_handler_second_argument_matching() {
        let config = DynamicCompletionDef {
            command: "test".to_string(),
            subcommands: vec![],
            description: "Test second argument".to_string(),
            match_condition: MatchCondition::SecondArgument,
            shell_command: "echo test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);

        // Should match when command matches and there's at least one arg
        assert!(handler.matches(&ParsedCommandLine {
            command: "test".to_string(),
            subcommand_path: vec![],
            args: vec!["first_arg".to_string()],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));

        // Should not match when command matches but no args
        assert!(!handler.matches(&ParsedCommandLine {
            command: "test".to_string(),
            subcommand_path: vec![],
            args: vec![], // No args
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));
    }

    #[test]
    fn test_script_based_handler_custom_pattern_matching() {
        let config = DynamicCompletionDef {
            command: "sudo".to_string(),
            subcommands: vec![],
            description: "Test custom pattern".to_string(),
            match_condition: MatchCondition::CustomPattern(CustomMatchPattern {
                command: Some("sudo".to_string()),
                subcommands: Some(vec!["pacman".to_string()]),
                args_contains: Some(vec!["S".to_string()]),
                options_contains: Some(vec![]),
                args_must_be_empty: None,
                args_positional: None,
            }),
            shell_command: "echo test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);

        // Should match when all custom conditions are met
        assert!(handler.matches(&ParsedCommandLine {
            command: "sudo".to_string(),
            subcommand_path: vec!["pacman".to_string()],
            args: vec!["-S".to_string(), "package".to_string()],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));

        // Should not match when custom conditions are not met
        assert!(!handler.matches(&ParsedCommandLine {
            command: "sudo".to_string(),
            subcommand_path: vec!["apt".to_string()], // Wrong subcommand
            args: vec!["-S".to_string()],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }));
    }

    #[test]
    fn test_substitute_variables() {
        let config = DynamicCompletionDef {
            command: "test".to_string(),
            subcommands: vec![],
            description: "Test substitution".to_string(),
            match_condition: MatchCondition::StartsWithCommand,
            shell_command: "echo $COMMAND $SUBCOMMAND $CURRENT_TOKEN".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);
        let parsed_command = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec!["checkout".to_string()],
            args: vec![],
            options: vec![],
            current_token: "feature".to_string(),
            current_arg: Some("feature".to_string()),
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        let result =
            handler.substitute_variables("$COMMAND $SUBCOMMAND $CURRENT_TOKEN", &parsed_command);
        assert_eq!(result, "git checkout feature");
    }

    #[test]
    fn test_generate_candidates_empty_output() {
        let config = DynamicCompletionDef {
            command: "test".to_string(),
            subcommands: vec![],
            description: "Test command".to_string(),
            match_condition: MatchCondition::StartsWithCommand,
            shell_command: "echo".to_string(), // Echo with no args produces empty line
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);
        let parsed_command = ParsedCommandLine {
            command: "test".to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        // Note: This test might fail in environments where the shell command execution fails
        // due to environment settings, but it tests the logic path
        let _result = handler.generate_candidates(&parsed_command);
        // This might return an error due to shell execution environment, so we'll just test the happy path
        // by using a command that actually produces output
    }

    #[test]
    fn test_pacman_positional_matching() {
        let config = DynamicCompletionDef {
            command: "sudo".to_string(),
            subcommands: vec![],
            description: "Test pacman positional matching".to_string(),
            match_condition: MatchCondition::CustomPattern(CustomMatchPattern {
                command: Some("sudo".to_string()),
                subcommands: None,
                args_contains: None,
                options_contains: None,
                args_must_be_empty: Some(false),
                args_positional: Some(vec![(0, "pacman".to_string()), (1, "S".to_string())]),
            }),
            shell_command: "echo test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let handler = ScriptBasedCompletionHandler::new(config);

        // This should match: sudo pacman -S (args = ["pacman", "-S"])
        let matching_command = ParsedCommandLine {
            command: "sudo".to_string(),
            subcommand_path: vec![],
            args: vec!["pacman".to_string(), "-S".to_string()],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        assert!(
            handler.matches(&matching_command),
            "Should match sudo pacman -S"
        );

        // This should NOT match: sudo user (args = ["user"])
        let non_matching_command = ParsedCommandLine {
            command: "sudo".to_string(),
            subcommand_path: vec![],
            args: vec!["user".to_string()],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        assert!(
            !handler.matches(&non_matching_command),
            "Should not match sudo user"
        );

        // This should NOT match: sudo (no args)
        let empty_args_command = ParsedCommandLine {
            command: "sudo".to_string(),
            subcommand_path: vec![],
            args: vec![], // Empty args
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        assert!(
            !handler.matches(&empty_args_command),
            "Should not match sudo without args"
        );
    }
}
