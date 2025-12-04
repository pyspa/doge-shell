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
mod export;
mod markdown;
pub use chatgpt::execute_chat_message;
mod dmv;
mod fg;
mod gco;
mod glog;
mod help;
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

    // New methods for export command
    fn list_exported_vars(&self) -> Vec<(String, String)>;
    fn export_var(&mut self, key: &str) -> bool;
    fn set_and_export_var(&mut self, key: String, value: String);
}

use std::any::Any;

/// Trait representing a builtin command with its description
pub trait BuiltinCommandTrait: Send + Sync {
    /// Execute the builtin command
    fn execute(&self, ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus;
    /// Get the description of the builtin command
    fn description(&self) -> &'static str;
    /// Get the command function directly
    fn as_any(&self) -> &dyn Any;
}

/// Implementation of the trait for function pointers
pub struct BuiltinCommandFn {
    pub func: fn(&Context, Vec<String>, &mut dyn ShellProxy) -> ExitStatus,
    pub description: &'static str,
}

impl BuiltinCommandFn {
    pub fn new(
        func: fn(&Context, Vec<String>, &mut dyn ShellProxy) -> ExitStatus,
        description: &'static str,
    ) -> Self {
        Self { func, description }
    }
}

/// Type alias for the builtin command function type to reduce complexity
type BuiltinFn = fn(&Context, Vec<String>, &mut dyn ShellProxy) -> ExitStatus;

impl BuiltinCommandTrait for BuiltinCommandFn {
    fn execute(&self, ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
        (self.func)(ctx, argv, proxy)
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Global registry of all builtin commands
/// Uses lazy initialization and mutex for thread-safe access
pub static BUILTIN_COMMAND: Lazy<Mutex<HashMap<&str, Box<dyn BuiltinCommandTrait>>>> =
    Lazy::new(|| {
        let mut builtin = HashMap::new();

        // Core shell commands
        builtin.insert(
            "exit",
            Box::new(BuiltinCommandFn::new(exit, exit_description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "cd",
            Box::new(BuiltinCommandFn::new(cd::command, cd::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "history",
            Box::new(BuiltinCommandFn::new(
                history::command,
                history::description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );

        // Navigation and directory management
        builtin.insert(
            "z",
            Box::new(BuiltinCommandFn::new(z::command, z::description()))
                as Box<dyn BuiltinCommandTrait>,
        );

        // Job control commands
        builtin.insert(
            "jobs",
            Box::new(BuiltinCommandFn::new(jobs::command, jobs::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "fg",
            Box::new(BuiltinCommandFn::new(fg::command, fg::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "bg",
            Box::new(BuiltinCommandFn::new(bg::command, bg::description()))
                as Box<dyn BuiltinCommandTrait>,
        );

        // Scripting and configuration
        builtin.insert(
            "lisp",
            Box::new(BuiltinCommandFn::new(lisp::command, lisp::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "set",
            Box::new(BuiltinCommandFn::new(set::command, set::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "var",
            Box::new(BuiltinCommandFn::new(var::command, var::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "read",
            Box::new(BuiltinCommandFn::new(read::command, read::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "abbr",
            Box::new(BuiltinCommandFn::new(abbr::command, abbr::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "alias",
            Box::new(BuiltinCommandFn::new(alias::command, alias::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "export",
            Box::new(BuiltinCommandFn::new(
                export::command,
                export::description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );

        // AI integration commands
        builtin.insert(
            "chat",
            Box::new(BuiltinCommandFn::new(
                chatgpt::chat,
                chatgpt::chat_description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "chat_prompt",
            Box::new(BuiltinCommandFn::new(
                chatgpt::chat_prompt,
                chatgpt::chat_prompt_description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "chat_model",
            Box::new(BuiltinCommandFn::new(
                chatgpt::chat_model,
                chatgpt::chat_model_description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );

        // Git integration commands
        builtin.insert(
            "glog",
            Box::new(BuiltinCommandFn::new(glog::command, glog::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "gco",
            Box::new(BuiltinCommandFn::new(gco::command, gco::description()))
                as Box<dyn BuiltinCommandTrait>,
        );

        // Utility commands
        builtin.insert(
            "add_path",
            Box::new(BuiltinCommandFn::new(
                add_path::command,
                add_path::description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "serve",
            Box::new(BuiltinCommandFn::new(serve::command, serve::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "uuid",
            Box::new(BuiltinCommandFn::new(uuid::command, uuid::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "dmv",
            Box::new(BuiltinCommandFn::new(dmv::command, dmv::description()))
                as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "reload",
            Box::new(BuiltinCommandFn::new(
                reload::command,
                reload::description(),
            )) as Box<dyn BuiltinCommandTrait>,
        );
        builtin.insert(
            "help",
            Box::new(BuiltinCommandFn::new(help::command, help::description()))
                as Box<dyn BuiltinCommandTrait>,
        );

        Mutex::new(builtin)
    });

/// Retrieves a builtin command function by name
/// Returns None if the command is not found
pub fn get_command(name: &str) -> Option<BuiltinFn> {
    if let Ok(builtin) = BUILTIN_COMMAND.lock() {
        // Find the BuiltinCommandFn inside the trait object and extract its func
        for (key, cmd) in builtin.iter() {
            if *key == name {
                // Since we created BuiltinCommandFn instances, we can downcast them
                if let Some(builtin_cmd) = cmd.as_any().downcast_ref::<BuiltinCommandFn>() {
                    return Some(builtin_cmd.func);
                }
            }
        }
    }
    None
}

/// Get all builtin commands with their descriptions
pub fn get_all_commands() -> Vec<(&'static str, &'static str)> {
    if let Ok(builtin) = BUILTIN_COMMAND.lock() {
        builtin
            .iter()
            .map(|(name, cmd)| (*name, cmd.description()))
            .collect()
    } else {
        Vec::new()
    }
}

/// Built-in exit command description
pub fn exit_description() -> &'static str {
    "Exit the shell"
}

/// Built-in exit command implementation
/// Initiates graceful shell termination
pub fn exit(_ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("Exit command called - initiating normal shell exit");
    proxy.exit_shell();
    ExitStatus::ExitedWith(0)
}
