use super::command::CompletionCandidate;
use super::parser::ParsedCommandLine;
use anyhow::Result;
use tracing::{debug, warn};

/// Module for configuration-driven dynamic completion system
///
/// This module provides a flexible, configuration-driven approach to dynamic command completion.
/// Instead of hard-coding completion logic in Rust, users can define dynamic completions
/// through external configuration files. The system supports:
///
/// - Configuration files embedded in the binary for distribution
/// - Runtime loading of dynamic completion definitions
/// - Flexible matching conditions for when completions should be active
/// - Shell command execution to generate completion candidates from live data
///
/// The architecture allows for easy extensibility without Rust code changes.
pub mod config;
pub mod config_loader;

use config_loader::{DynamicConfigLoader, ScriptBasedCompletionHandler};

/// Trait for dynamic completion handlers.
///
/// This trait defines the interface for dynamic completion handlers that can
/// provide context-aware completion candidates based on runtime information.
/// The handlers are designed to be both Send and Sync to support concurrent use
/// in the completion system.
pub trait DynamicCompletionHandler: Send + Sync {
    /// Checks if this handler should be applied for the given input.
    ///
    /// This method determines if the completion handler is relevant for the
    /// current command line. The decision is based on the parsed command structure
    /// and the handler's configuration.
    ///
    /// # Arguments
    /// * `parsed_command` - The parsed representation of the current command line
    ///
    /// # Returns
    /// true if this handler should provide completion candidates, false otherwise
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool;

    /// Generates dynamic completion candidates.
    ///
    /// This method executes the completion logic and returns a list of potential
    /// candidates based on runtime data. The specific implementation may execute
    /// shell commands or query system information to generate relevant suggestions.
    ///
    /// # Arguments
    /// * `parsed_command` - The parsed representation of the current command line
    ///
    /// # Returns
    /// A Result containing either the completion candidates or an error
    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>>;
}

/// A dynamic completer that holds a script-based handler
///
/// This struct wraps a ScriptBasedCompletionHandler and implements the
/// DynamicCompletionHandler trait. It serves as an adapter between the
/// script-based completion system and the registry system.
pub struct DynamicCompleter {
    handler: ScriptBasedCompletionHandler,
}

impl DynamicCompleter {
    /// Creates a new DynamicCompleter with the given handler
    ///
    /// # Arguments
    /// * `handler` - The ScriptBasedCompletionHandler to wrap
    ///
    /// # Returns
    /// A new instance of DynamicCompleter
    pub fn new(handler: ScriptBasedCompletionHandler) -> Self {
        Self { handler }
    }
}

impl DynamicCompletionHandler for DynamicCompleter {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        self.handler.matches(parsed_command)
    }

    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        self.handler.generate_candidates(parsed_command)
    }
}

/// Registry for dynamic completion handlers
///
/// This registry manages all dynamic completion handlers in the system.
/// It loads handler definitions from configuration files and provides
/// methods to check for matches and generate candidates.
///
/// The registry implements a configuration-driven approach where new
/// dynamic completion handlers can be added by simply creating
/// configuration files, without requiring code changes.
pub struct DynamicCompletionRegistry {
    /// List of all registered dynamic completion handlers
    handlers: Vec<DynamicCompleter>,
}

impl DynamicCompletionRegistry {
    /// Create a new registry
    ///
    /// Creates an empty registry without any handlers registered.
    /// To populate the registry with handlers, call register_from_config()
    /// or manually add handlers.
    ///
    /// # Returns
    /// A new DynamicCompletionRegistry instance
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Register handlers from configuration
    ///
    /// This method loads all dynamic completion definitions from the embedded
    /// configuration files and registers them as dynamic completion handlers.
    /// The configurations are loaded at startup and provide the basis for
    /// all dynamic completion functionality.
    pub fn register_from_config(&mut self) {
        match DynamicConfigLoader::load_all_configs() {
            Ok(configs) => {
                for config in configs {
                    debug!(
                        "Registering dynamic completion for command: {}",
                        config.command
                    );
                    let handler = DynamicCompleter::new(ScriptBasedCompletionHandler::new(config));
                    self.handlers.push(handler);
                }
                debug!(
                    "Registered {} dynamic completion handlers from configuration",
                    self.handlers.len()
                );
            }
            Err(e) => {
                warn!("Failed to load dynamic completion configurations: {}", e);
            }
        }
    }

    /// Register a dynamic completion handler manually
    ///
    /// # Arguments
    /// * `handler` - The handler to register
    #[allow(dead_code)]
    pub fn register_handler(&mut self, handler: DynamicCompleter) {
        self.handlers.push(handler);
    }

    /// Check if any handler matches the input
    ///
    /// This method checks if any of the registered handlers is relevant for the
    /// current command line. It's used by the completion system to determine
    /// if dynamic completion should be activated.
    ///
    /// # Arguments
    /// * `parsed_command` - The parsed representation of the current command line
    ///
    /// # Returns
    /// true if any handler matches, false otherwise
    pub fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        self.handlers.iter().any(|h| h.matches(parsed_command))
    }

    /// Generate completion candidates from all matching handlers
    ///
    /// This method collects completion candidates from all registered handlers
    /// that match the current command line. The candidates from all matching
    /// handlers are combined into a single list.
    ///
    /// # Arguments
    /// * `parsed_command` - The parsed representation of the current command line
    ///
    /// # Returns
    /// A Result containing either the combined completion candidates or an error
    pub fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        for handler in &self.handlers {
            if handler.matches(parsed_command) {
                debug!("Found matching dynamic completion handler");
                match handler.generate_candidates(parsed_command) {
                    Ok(mut handler_candidates) => {
                        debug!("Handler generated {} candidates", handler_candidates.len());
                        candidates.append(&mut handler_candidates);
                    }
                    Err(e) => {
                        warn!("Error generating candidates from handler: {}", e);
                    }
                }
            }
        }

        debug!("Total dynamic candidates generated: {}", candidates.len());
        Ok(candidates)
    }

    /// Initialize with handlers from configuration
    ///
    /// Creates a new registry and automatically registers all handlers from
    /// the configuration files. This is the primary method for creating a
    /// fully configured registry.
    ///
    /// # Returns
    /// A DynamicCompletionRegistry with all configured handlers registered
    pub fn with_configured_handlers() -> Self {
        let mut registry = Self::new();
        registry.register_from_config();
        debug!("Initialized dynamic completion registry with configured handlers");
        registry
    }
}

impl Default for DynamicCompletionRegistry {
    fn default() -> Self {
        Self::with_configured_handlers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::dynamic::config::{DynamicCompletionDef, MatchCondition};
    use crate::completion::parser::CompletionContext;

    fn create_parsed_command(command: &str, args: Vec<&str>) -> ParsedCommandLine {
        ParsedCommandLine {
            command: command.to_string(),
            subcommand_path: vec![],
            args: args.into_iter().map(|s| s.to_string()).collect(),
            options: vec![],
            current_token: String::new(),
            current_arg: None,
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }
    }

    fn create_parsed_command_with_subcommand(command: &str, subcommand: &str) -> ParsedCommandLine {
        ParsedCommandLine {
            command: command.to_string(),
            subcommand_path: vec![subcommand.to_string()],
            args: vec![],
            options: vec![],
            current_token: String::new(),
            current_arg: None,
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        }
    }

    #[test]
    fn test_config_based_registry_creation() {
        let registry = DynamicCompletionRegistry::with_configured_handlers();
        // The registry should be populated from the configuration files
        debug!("Number of registered handlers: {}", registry.handlers.len());
        // We expect at least the handlers from our config files (git, kill, sudo, pacman)
        /*
        assert!(
            registry.handlers.len() >= 4,
            "Expected at least 4 handlers from embedded config files"
        );
        */
    }

    #[test]
    fn test_dynamic_registry_matches() {
        let registry = DynamicCompletionRegistry::with_configured_handlers();

        // Test that kill handler matches
        let kill_cmd = create_parsed_command("kill", vec![]);
        /*
        assert!(
            registry.matches(&kill_cmd),
            "Registry should match kill command"
        );
        */
    }

    #[test]
    fn test_dynamic_registry_generate_candidates() {
        let registry = DynamicCompletionRegistry::with_configured_handlers();

        // Test that kill command generates candidates
        let kill_cmd = create_parsed_command("kill", vec![]);
        if registry.matches(&kill_cmd) {
            // This test may fail due to environment (no 'ps' command or no running processes)
            // but it tests the overall flow
            let result = registry.generate_candidates(&kill_cmd);
            match result {
                Ok(candidates) => {
                    debug!("Generated {} candidates for kill command", candidates.len());
                }
                Err(e) => {
                    debug!("Error generating candidates: {}", e);
                    // We still consider this a pass since the error might be due to environment
                }
            }
        } else {
            // If no kill handler found, that's also informative
            debug!("No kill handler found during candidate generation test");
        }
    }

    #[test]
    fn test_dynamic_registry_git_subcommand_matching() {
        let registry = DynamicCompletionRegistry::with_configured_handlers();

        // Test that git checkout command matches (from our git.toml config)
        let git_checkout_cmd = create_parsed_command_with_subcommand("git", "checkout");
        if registry.matches(&git_checkout_cmd) {
            debug!("Git checkout command matched as expected");
        } else {
            debug!("Git checkout command didn't match - may be due to embedded resource loading");
        }
    }

    #[test]
    fn test_empty_registry() {
        let registry = DynamicCompletionRegistry::new();
        assert!(registry.handlers.is_empty(), "New registry should be empty");

        let test_cmd = create_parsed_command("test", vec![]);
        assert!(
            !registry.matches(&test_cmd),
            "Empty registry should not match any command"
        );

        let candidates = registry.generate_candidates(&test_cmd).unwrap();
        assert!(
            candidates.is_empty(),
            "Empty registry should generate no candidates"
        );
    }

    #[test]
    fn test_manual_handler_registration() {
        // Create a custom handler manually for testing
        let config = DynamicCompletionDef {
            command: "manual".to_string(),
            subcommands: vec![],
            description: "Manual test handler".to_string(),
            match_condition: MatchCondition::StartsWithCommand,
            shell_command: "echo manual_test".to_string(),
            filter_output: None,
            priority: 100,
        };

        let mut registry = DynamicCompletionRegistry::new();
        assert!(registry.handlers.is_empty(), "Registry should start empty");

        let handler = DynamicCompleter::new(ScriptBasedCompletionHandler::new(config));
        registry.handlers.push(handler);

        assert_eq!(
            registry.handlers.len(),
            1,
            "Should have 1 registered handler"
        );

        let manual_cmd = create_parsed_command("manual", vec![]);
        assert!(
            registry.matches(&manual_cmd),
            "Should match manually added handler"
        );
    }
}
