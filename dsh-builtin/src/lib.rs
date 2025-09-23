use anyhow::Result;
use dsh_types::{Context, ExitStatus, mcp::McpServerConfig};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::debug;

// Builtin command modules
mod abbr;
mod add_path;
mod alias;
mod bg;
pub mod cd;
mod chatgpt;
pub use chatgpt::execute_chat_message;
mod dmv;
mod fg;
mod gco;
mod glog;
mod history;
mod jobs;
pub mod lisp;
mod read;
mod reload;
pub mod serve;
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

    /// Retrieves an alias command by name
    fn get_alias(&mut self, name: &str) -> Option<String>;

    /// Sets an alias mapping from name to command
    fn set_alias(&mut self, name: String, command: String);

    /// Lists all current aliases as a HashMap
    fn list_aliases(&mut self) -> std::collections::HashMap<String, String>;

    /// Adds a new abbreviation
    fn add_abbr(&mut self, name: String, expansion: String);

    /// Removes an abbreviation by name, returns true if it existed
    fn remove_abbr(&mut self, name: &str) -> bool;

    /// Lists all abbreviations as name-expansion pairs
    fn list_abbrs(&self) -> Vec<(String, String)>;

    /// Gets an abbreviation expansion by name
    fn get_abbr(&self, name: &str) -> Option<String>;

    /// Lists MCP servers configured in the shell session
    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig>;

    /// Lists execute-tool allowlist entries configured via config.lisp
    fn list_execute_allowlist(&mut self) -> Vec<String>;
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
    builtin.insert("abbr", abbr::command as BuiltinCommand);
    builtin.insert("alias", alias::command as BuiltinCommand);

    // AI integration commands
    builtin.insert("chat", chatgpt::chat as BuiltinCommand);
    builtin.insert("chat_prompt", chatgpt::chat_prompt as BuiltinCommand);
    builtin.insert("chat_model", chatgpt::chat_model as BuiltinCommand);

    // Git integration commands
    builtin.insert("glog", glog::command as BuiltinCommand);
    builtin.insert("gco", gco::command as BuiltinCommand);

    // Utility commands
    builtin.insert("add_path", add_path::command as BuiltinCommand);
    builtin.insert("serve", serve::command as BuiltinCommand);
    builtin.insert("uuid", uuid::command as BuiltinCommand);
    builtin.insert("dmv", dmv::command as BuiltinCommand);
    builtin.insert("reload", reload::command as BuiltinCommand);

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
