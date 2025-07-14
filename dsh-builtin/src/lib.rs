use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::debug;

// Builtin command modules
mod add_path;
mod bg;
pub mod cd;
mod chatgpt;
mod fg;
mod history;
mod jobs;
pub mod lisp;
mod read;
mod set;
mod uuid;
mod var;
mod z;

/// Trait that provides an interface for builtin commands to interact with the shell
/// This allows builtin commands to perform shell operations without direct coupling
pub trait ShellProxy {
    /// Initiates shell exit process
    fn exit_shell(&mut self);

    /// Dispatches a command to the shell's command execution system
    /// Used for commands that need to be handled by the main shell logic
    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;

    /// Records a path in the shell's path history for frecency-based navigation
    fn save_path_history(&mut self, path: &str);

    /// Changes the current working directory and updates shell state
    fn changepwd(&mut self, path: &str) -> Result<()>;

    /// Inserts a path at the specified index in the PATH environment variable
    fn insert_path(&mut self, index: usize, path: &str);

    /// Retrieves a shell variable value by key
    fn get_var(&mut self, key: &str) -> Option<String>;

    /// Sets a shell variable (local to the shell session)
    fn set_var(&mut self, key: String, value: String);

    /// Sets an environment variable (exported to child processes)
    fn set_env_var(&mut self, key: String, value: String);
}

/// Type alias for builtin command function signature
/// All builtin commands must conform to this signature
pub type BuiltinCommand =
    fn(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus;

/// Global registry of all builtin commands
/// Uses lazy initialization and mutex for thread-safe access
pub static BUILTIN_COMMAND: Lazy<Mutex<HashMap<&str, BuiltinCommand>>> = Lazy::new(|| {
    let mut builtin = HashMap::new();

    // Core shell commands
    builtin.insert("exit", exit as BuiltinCommand);
    builtin.insert("cd", cd::command as BuiltinCommand);
    builtin.insert("history", history::command as BuiltinCommand);

    // Navigation and directory management
    builtin.insert("z", z::command as BuiltinCommand);

    // Job control commands
    builtin.insert("jobs", jobs::command as BuiltinCommand);
    builtin.insert("fg", fg::command as BuiltinCommand);
    builtin.insert("bg", bg::command as BuiltinCommand);

    // Scripting and configuration
    builtin.insert("lisp", lisp::command as BuiltinCommand);
    builtin.insert("set", set::command as BuiltinCommand);
    builtin.insert("var", var::command as BuiltinCommand);
    builtin.insert("read", read::command as BuiltinCommand);

    // AI integration commands
    builtin.insert("chat", chatgpt::chat as BuiltinCommand);
    builtin.insert("chat_prompt", chatgpt::chat_prompt as BuiltinCommand);

    // Utility commands
    builtin.insert("add_path", add_path::command as BuiltinCommand);
    builtin.insert("uuid", uuid::command as BuiltinCommand);

    Mutex::new(builtin)
});

/// Retrieves a builtin command function by name
/// Returns None if the command is not found
pub fn get_command(name: &str) -> Option<BuiltinCommand> {
    if let Ok(builtin) = BUILTIN_COMMAND.lock() {
        builtin.get(name).copied()
    } else {
        None
    }
}

/// Built-in exit command implementation
/// Initiates graceful shell termination
pub fn exit(_ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("Exit command called - initiating normal shell exit");
    proxy.exit_shell();
    ExitStatus::ExitedWith(0)
}
