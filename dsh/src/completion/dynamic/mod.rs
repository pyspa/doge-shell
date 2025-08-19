use super::command::CompletionCandidate;
use super::parser::ParsedCommandLine;
use anyhow::Result;
use tracing::{debug, warn};

mod git;
mod kill;
mod pacman;
mod sudo;

/// Trait for dynamic completion handlers.
pub trait DynamicCompletionHandler: Send + Sync {
    /// Checks if this handler should be applied for the given input.
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool;

    /// Generates dynamic completion candidates.
    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>>;
}

/// Enum to hold all possible dynamic completion handlers
pub enum DynamicCompleter {
    Kill(kill::KillCompletionHandler),
    Sudo(sudo::SudoCompletionHandler),
    Git(git::GitCompletionHandler),
    Pacman(pacman::PacmanCompletionHandler),
}

impl DynamicCompletionHandler for DynamicCompleter {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        match self {
            DynamicCompleter::Kill(handler) => handler.matches(parsed_command),
            DynamicCompleter::Sudo(handler) => handler.matches(parsed_command),
            DynamicCompleter::Git(handler) => handler.matches(parsed_command),
            DynamicCompleter::Pacman(handler) => handler.matches(parsed_command),
        }
    }

    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        match self {
            DynamicCompleter::Kill(handler) => handler.generate_candidates(parsed_command),
            DynamicCompleter::Sudo(handler) => handler.generate_candidates(parsed_command),
            DynamicCompleter::Git(handler) => handler.generate_candidates(parsed_command),
            DynamicCompleter::Pacman(handler) => handler.generate_candidates(parsed_command),
        }
    }
}

/// Registry for dynamic completion handlers
pub struct DynamicCompletionRegistry {
    handlers: Vec<DynamicCompleter>,
}

impl DynamicCompletionRegistry {
    /// Create a new registry
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Register a kill completion handler
    pub fn register_kill(&mut self) {
        debug!("Registering kill completion handler");
        self.handlers
            .push(DynamicCompleter::Kill(kill::KillCompletionHandler));
    }

    /// Register a sudo completion handler
    pub fn register_sudo(&mut self) {
        debug!("Registering sudo completion handler");
        self.handlers
            .push(DynamicCompleter::Sudo(sudo::SudoCompletionHandler));
    }

    /// Register a git completion handler
    pub fn register_git(&mut self) {
        debug!("Registering git completion handler");
        self.handlers
            .push(DynamicCompleter::Git(git::GitCompletionHandler));
    }

    /// Register a pacman completion handler
    pub fn register_pacman(&mut self) {
        debug!("Registering pacman completion handler");
        self.handlers
            .push(DynamicCompleter::Pacman(pacman::PacmanCompletionHandler));
    }

    /// Check if any handler matches the input
    pub fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        self.handlers.iter().any(|h| h.matches(parsed_command))
    }

    /// Generate completion candidates from all matching handlers
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

    /// Initialize with default handlers
    pub fn with_default_handlers() -> Self {
        let mut registry = Self::new();
        registry.register_kill();
        registry.register_sudo();
        registry.register_git();
        registry.register_pacman();
        debug!("Initialized dynamic completion registry with default handlers");
        registry
    }
}

impl Default for DynamicCompletionRegistry {
    fn default() -> Self {
        Self::with_default_handlers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CompletionContext;

    fn create_parsed_command(command: &str, args: Vec<&str>) -> ParsedCommandLine {
        ParsedCommandLine {
            command: command.to_string(),
            args: args.into_iter().map(|s| s.to_string()).collect(),
            current_arg: None,
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            cursor_index: 0,
        }
    }

    fn create_parsed_command_with_current_arg(
        command: &str,
        args: Vec<&str>,
        current_arg: Option<&str>,
    ) -> ParsedCommandLine {
        ParsedCommandLine {
            command: command.to_string(),
            args: args.into_iter().map(|s| s.to_string()).collect(),
            current_arg: current_arg.map(|s| s.to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            cursor_index: 0,
        }
    }

    #[test]
    fn test_kill_completion_handler_matches() {
        let handler = kill::KillCompletionHandler;
        assert!(handler.matches(&create_parsed_command("kill", vec![])));
        assert!(handler.matches(&create_parsed_command("kill", vec!["123"])));
        assert!(!handler.matches(&create_parsed_command("killall", vec![])));
        assert!(!handler.matches(&create_parsed_command("kill", vec!["-9", "123"])));
    }

    #[test]
    fn test_sudo_completion_handler_matches() {
        let handler = sudo::SudoCompletionHandler;
        assert!(handler.matches(&create_parsed_command("sudo", vec![])));
        assert!(!handler.matches(&create_parsed_command("sudo", vec!["user"])));
        assert!(!handler.matches(&create_parsed_command("sudoku", vec![])));
    }

    #[test]
    fn test_git_completion_handler_matches() {
        let handler = git::GitCompletionHandler;
        assert!(handler.matches(&create_parsed_command("git", vec!["switch"])));
        assert!(handler.matches(&create_parsed_command("git", vec!["checkout"])));
        assert!(!handler.matches(&create_parsed_command("git", vec!["commit"])));
        assert!(!handler.matches(&create_parsed_command("github", vec!["switch"])));
    }

    #[test]
    fn test_pacman_completion_handler_matches() {
        let handler = pacman::PacmanCompletionHandler;
        assert!(handler.matches(&create_parsed_command("sudo", vec!["pacman", "-S"])));
        assert!(!handler.matches(&create_parsed_command("sudo", vec!["pacman", "-R"])));
        assert!(!handler.matches(&create_parsed_command("yay", vec!["pacman", "-S"])));

        // Test with trailing space (current_arg is empty)
        assert!(handler.matches(&create_parsed_command_with_current_arg(
            "sudo",
            vec!["pacman", "-S"],
            Some("")
        )));

        // Test with partial argument - should still match to provide filtered completions
        assert!(handler.matches(&create_parsed_command_with_current_arg(
            "sudo",
            vec!["pacman", "-S"],
            Some("part")
        )));
    }

    #[test]
    fn test_dynamic_registry_creation() {
        let registry = DynamicCompletionRegistry::with_default_handlers();

        // Should have all default handlers
        assert_eq!(registry.handlers.len(), 4);

        // Should match kill command
        assert!(registry.matches(&create_parsed_command("kill", vec![])));

        // Should match sudo command
        assert!(registry.matches(&create_parsed_command("sudo", vec![])));

        // Should match git command
        assert!(registry.matches(&create_parsed_command("git", vec!["switch"])));

        // Should match pacman command
        assert!(registry.matches(&create_parsed_command("sudo", vec!["pacman", "-S"])));
    }

    #[test]
    fn test_dynamic_completer_matches() {
        let kill_completer = DynamicCompleter::Kill(kill::KillCompletionHandler);
        let sudo_completer = DynamicCompleter::Sudo(sudo::SudoCompletionHandler);
        let git_completer = DynamicCompleter::Git(git::GitCompletionHandler);
        let pacman_completer = DynamicCompleter::Pacman(pacman::PacmanCompletionHandler);

        assert!(kill_completer.matches(&create_parsed_command("kill", vec![])));
        assert!(!kill_completer.matches(&create_parsed_command("sudo", vec![])));

        assert!(sudo_completer.matches(&create_parsed_command("sudo", vec![])));
        assert!(!sudo_completer.matches(&create_parsed_command("kill", vec![])));

        assert!(git_completer.matches(&create_parsed_command("git", vec!["switch"])));
        assert!(!git_completer.matches(&create_parsed_command("kill", vec![])));

        assert!(pacman_completer.matches(&create_parsed_command("sudo", vec!["pacman", "-S"])));
        assert!(!pacman_completer.matches(&create_parsed_command("kill", vec![])));
    }
}
