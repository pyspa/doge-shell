use super::command::CompletionCandidate;
use super::parser::ParsedCommandLine;
use anyhow::Result;
use tracing::{debug, warn};

mod kill;
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
}

impl DynamicCompletionHandler for DynamicCompleter {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        match self {
            DynamicCompleter::Kill(handler) => handler.matches(parsed_command),
            DynamicCompleter::Sudo(handler) => handler.matches(parsed_command),
        }
    }

    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        match self {
            DynamicCompleter::Kill(handler) => handler.generate_candidates(parsed_command),
            DynamicCompleter::Sudo(handler) => handler.generate_candidates(parsed_command),
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

    /// Check if any handler matches the input
    pub fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        self.handlers.iter().any(|h| h.matches(parsed_command))
    }

    /// Generate completion candidates from all matching handlers
    pub async fn generate_candidates(
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
        debug!("Initialized dynamic completion registry with default handlers");
        registry
    }
}

impl Default for DynamicCompletionRegistry {
    fn default() -> Self {
        Self::with_default_handlers()
    }
}

// /// Handler for `kill` command dynamic completion.
// pub struct KillCompletionHandler;

// impl DynamicCompletionHandler for KillCompletionHandler {
//     fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
//         parsed_command.command == "kill" && parsed_command.args.len() <= 1
//     }

//     fn generate_candidates(
//         &self,
//         _parsed_command: &ParsedCommandLine,
//     ) -> Result<Vec<CompletionCandidate>> {
//         debug!("Generating dynamic completion candidates for 'kill' command.");

//         // Since we can't use async directly, we'll use tokio::task::block_in_place
//         // to run the async code in a blocking context
//         let output = tokio::task::block_in_place(|| {
//             tokio::runtime::Handle::current()
//                 .block_on(async { Command::new("ps").arg("-eo").arg("pid,comm").output().await })
//         })?;

//         let stdout = String::from_utf8_lossy(&output.stdout);
//         let mut candidates = Vec::new();

//         for line in stdout.lines().skip(1) {
//             let parts: Vec<&str> = line.trim().splitn(2, ' ').collect();
//             if parts.len() == 2 {
//                 let pid = parts[0].trim();
//                 let comm = parts[1].trim();
//                 candidates.push(CompletionCandidate {
//                     text: pid.to_string(),
//                     description: Some(format!("Process: {comm}")),
//                     completion_type: super::command::CompletionType::Argument,
//                     priority: 100,
//                 });
//             }
//         }
//         debug!(
//             "Generated {} candidates for 'kill' command.",
//             candidates.len()
//         );
//         Ok(candidates)
//     }
// }

// /// Handler for `sudo` command dynamic completion.
// pub struct SudoCompletionHandler;

// impl DynamicCompletionHandler for SudoCompletionHandler {
//     fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
//         parsed_command.command == "sudo" && parsed_command.args.is_empty()
//     }

//     fn generate_candidates(
//         &self,
//         _parsed_command: &ParsedCommandLine,
//     ) -> Result<Vec<CompletionCandidate>> {
//         debug!("Generating dynamic completion candidates for 'sudo' command.");

//         // Since we can't use async directly, we'll use tokio::task::block_in_place
//         // to run the async code in a blocking context
//         let output = tokio::task::block_in_place(|| {
//             tokio::runtime::Handle::current()
//                 .block_on(async { Command::new("getent").arg("passwd").output().await })
//         })?;

//         let stdout = String::from_utf8_lossy(&output.stdout);
//         let mut candidates = Vec::new();

//         for line in stdout.lines() {
//             let parts: Vec<&str> = line.split(':').collect();
//             if let Some(username) = parts.first() {
//                 candidates.push(CompletionCandidate {
//                     text: username.to_string(),
//                     description: None,
//                     completion_type: super::command::CompletionType::Argument,
//                     priority: 100,
//                 });
//             }
//         }
//         debug!(
//             "Generated {} candidates for 'sudo' command.",
//             candidates.len()
//         );
//         Ok(candidates)
//     }
// }

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
    fn test_dynamic_registry_creation() {
        let registry = DynamicCompletionRegistry::with_default_handlers();

        // Should have both default handlers
        assert_eq!(registry.handlers.len(), 2);

        // Should match kill command
        assert!(registry.matches(&create_parsed_command("kill", vec![])));

        // Should match sudo command
        assert!(registry.matches(&create_parsed_command("sudo", vec![])));
    }

    #[test]
    fn test_dynamic_completer_matches() {
        let kill_completer = DynamicCompleter::Kill(kill::KillCompletionHandler);
        let sudo_completer = DynamicCompleter::Sudo(sudo::SudoCompletionHandler);

        assert!(kill_completer.matches(&create_parsed_command("kill", vec![])));
        assert!(!kill_completer.matches(&create_parsed_command("sudo", vec![])));

        assert!(sudo_completer.matches(&create_parsed_command("sudo", vec![])));
        assert!(!sudo_completer.matches(&create_parsed_command("kill", vec![])));
    }
}
