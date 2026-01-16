//! Builtin command registry.
//!
//! Provides a centralized registry for all builtin shell commands,
//! making it easy to add new commands without modifying the dispatch function.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use once_cell::sync::Lazy;
use std::collections::HashMap;

use super::{exit, history, jobs, lisp, reload, var, z};

/// Type alias for builtin command handler functions.
pub type CommandHandler = fn(&mut Shell, &Context, Vec<String>) -> Result<()>;

/// Global builtin command registry.
pub static BUILTIN_REGISTRY: Lazy<BuiltinRegistry> = Lazy::new(BuiltinRegistry::new);

/// Registry of builtin shell commands.
pub struct BuiltinRegistry {
    commands: HashMap<&'static str, CommandHandler>,
}

impl BuiltinRegistry {
    /// Create a new registry with all builtin commands registered.
    pub fn new() -> Self {
        let mut commands: HashMap<&'static str, CommandHandler> = HashMap::new();

        // Core commands
        commands.insert("exit", exit::execute);
        commands.insert("history", history::execute);
        commands.insert("reload", reload::execute);

        // Navigation
        commands.insert("z", z::execute);

        // Job control
        commands.insert("jobs", jobs::execute_jobs);
        commands.insert("fg", jobs::execute_fg);
        commands.insert("bg", jobs::execute_bg);

        // Lisp
        commands.insert("lisp", lisp::execute_lisp);
        commands.insert("lisp-run", lisp::execute_lisp_run);

        // Variables
        commands.insert("var", var::execute_var);
        commands.insert("read", var::execute_read);

        Self { commands }
    }

    /// Get a command handler by name.
    pub fn get(&self, name: &str) -> Option<&CommandHandler> {
        self.commands.get(name)
    }

    /// Check if a command is a builtin.
    #[allow(dead_code)]
    pub fn is_builtin(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// List all registered builtin command names.
    #[allow(dead_code)]
    pub fn list(&self) -> Vec<&'static str> {
        self.commands.keys().copied().collect()
    }
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_contains_exit() {
        assert!(BUILTIN_REGISTRY.is_builtin("exit"));
    }

    #[test]
    fn test_registry_contains_all_commands() {
        let expected = vec![
            "exit", "history", "reload", "z", "jobs", "fg", "bg", "lisp", "lisp-run", "var", "read",
        ];
        for cmd in expected {
            assert!(
                BUILTIN_REGISTRY.is_builtin(cmd),
                "Expected builtin '{}' not found",
                cmd
            );
        }
    }

    #[test]
    fn test_registry_external_not_builtin() {
        assert!(!BUILTIN_REGISTRY.is_builtin("ls"));
        assert!(!BUILTIN_REGISTRY.is_builtin("git"));
    }

    #[test]
    fn test_registry_list() {
        let list = BUILTIN_REGISTRY.list();
        assert!(list.len() >= 11);
    }
}
