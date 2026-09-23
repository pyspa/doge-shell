//! Builtin command crate.
//!
//! The crate root exposes the ShellProxy compatibility surface and public
//! builtin API while command metadata and registration live in `registry`.

// doge-shell targets exactly two platforms. `#[cfg(not(target_os = "macos"))]`
// arms across the tree are written to mean "Linux"; this is what makes that
// reading true instead of merely usual.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("doge-shell supports Linux and macOS only");

use anyhow::Result;
use dsh_types::{
    Context, ExitStatus, command_block::CommandBlock, mcp::McpServerConfig,
    output_history::OutputEntry, safety_policy::SafetyLevel,
};
use std::future::Future;
use std::pin::Pin;
use tracing::debug;

// Builtin command modules
mod abbr;
mod add_path;
mod ai_watch;

pub mod agent;
mod alias;
mod background;
mod bg;
mod blocks;
pub mod cd;
mod chatgpt;
pub mod config_paths;
pub mod cron;
mod dashboard;
pub(crate) mod diff;
mod doctor;
mod eproject;
mod eview;
mod export;
mod include;
mod interactive_input;
mod magit;
mod markdown;
mod safe_run;
mod safety_policy;
mod skill;
pub use background::{BackgroundBuiltinMode, background_builtin_mode};
pub use chatgpt::chat_jobs_shutdown;
pub use chatgpt::chat_status_detailed;
pub use chatgpt::execute_chat_message;
pub use chatgpt::{
    LEGACY_SSE_UNSUPPORTED_MESSAGE, McpConnectionStatus, McpManager, McpRuntimeStateSnapshot,
    McpServerStatus, McpToolExposure, McpToolGroup,
};
pub use registry::{
    AsyncBuiltinFn, BUILTIN_COMMAND, BuiltinFn, BuiltinFuture, BuiltinHandler, BuiltinSpec,
    get_all_commands, get_command, get_handler, is_builtin,
};
pub use skill::installed_names as installed_skill_names;
pub use skill::pending_proposal_ids;
mod atomic_write;
mod bookmark;
pub mod capability;
pub mod command_timing;
mod commit_ai;
pub mod comp_gen;
pub mod completion_generation;
pub mod dirstack;
mod dmv;
mod fg;
pub mod ga;
mod gco;
pub mod gh_notify;
mod github_client;
mod glog;
mod gpr;
mod gwt;
mod help;
mod history;
mod jobs;
pub mod lisp;
mod mcp;
mod notebook_play;
mod out;
pub mod output_gen;
pub mod procs;
pub mod project;
pub mod project_context;
mod read;
mod registry;
mod removed_sched;
mod runbook;
pub mod shell_capabilities;
#[cfg(test)]
pub(crate) mod test_support;

mod reload;
pub mod serve;
mod set;
mod skim_runner;
mod snippet;
pub mod task;
mod text;
pub mod tm;
mod trigger;
mod uuid;
mod var;
mod wait;
mod z;

/// Shell-owned operations that cannot be implemented inside `dsh-builtin`.
///
/// Public builtin names remain strings at the CLI boundary, but the handoff to
/// the shell core is exhaustive and typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreShellAction {
    ChatStatus,
    Cron,
    Exit,
    History,
    Reload,
    Z,
    BlocksTui,
    BlocksPersistent,
    Jobs,
    Foreground,
    Background,
    Lisp,
    LispRun,
    Var,
    AbbrCommand,
}

impl CoreShellAction {
    pub const fn command_name(self) -> &'static str {
        match self {
            Self::ChatStatus => "chat_status",
            Self::Cron => "cron",
            Self::Exit => "exit",
            Self::History => "history",
            Self::Reload => "reload",
            Self::Z => "z",
            Self::BlocksTui => "blocks-tui",
            Self::BlocksPersistent => "blocks-persistent",
            Self::Jobs => "jobs",
            Self::Foreground => "fg",
            Self::Background => "bg",
            Self::Lisp => "lisp",
            Self::LispRun => "lisp-run",
            Self::Var => "var",
            Self::AbbrCommand => "abbr-command",
        }
    }

    pub fn from_command_name(command: &str) -> Option<Self> {
        Some(match command {
            "chat_status" => Self::ChatStatus,
            "cron" => Self::Cron,
            "exit" => Self::Exit,
            "history" => Self::History,
            "reload" => Self::Reload,
            "z" => Self::Z,
            "blocks-tui" => Self::BlocksTui,
            "blocks-persistent" => Self::BlocksPersistent,
            "jobs" => Self::Jobs,
            "fg" => Self::Foreground,
            "bg" => Self::Background,
            "lisp" => Self::Lisp,
            "lisp-run" => Self::LispRun,
            "var" => Self::Var,
            "abbr-command" => Self::AbbrCommand,
            _ => return None,
        })
    }
}

/// Trait that provides an interface for builtin commands to interact with the shell
/// This allows builtin commands to perform shell operations without direct coupling
///
/// No method here has a default body, deliberately. There are exactly two
/// implementors - the real `Shell` (`dsh/src/proxy/mod.rs`) and the shared
/// test double [`crate::test_support::TestShellProxy`] - and both provide
/// every method explicitly. A default body would let either one silently
/// fall back to a no-op if a method were forgotten after a signature change;
/// requiring every method keeps that a compile error instead.
///
/// `: AgentCommandPolicy` is a supertrait bound, not a new `ShellProxy`
/// method - it adds no entry to `MAX_COMPATIBILITY_METHODS` and both
/// implementors already satisfy it via their own `impl AgentCommandPolicy`.
/// It exists so a plain `&mut dyn ShellProxy` (what every sync builtin
/// receives) can call `evaluate_agent_command`/`request_agent_approval`
/// without the caller having to know about `AgentCommandPolicy` by name.
/// `safe_run.rs` uses this so a command it is about to execute is judged by
/// the same `SafetyGuard` as a command typed directly - previously the
/// dispatch below it (`proxy::external::execute`) never consulted the guard
/// at all.
///
/// `: ... + AiJsonRequest` is the same kind of addition, for the same
/// reason: `output_gen.rs` needs `ask_ai_json_async` reachable through a
/// plain `&mut dyn ShellProxy`.
pub trait ShellProxy:
    shell_capabilities::AgentCommandPolicy
    + shell_capabilities::AiJsonRequest
    + shell_capabilities::JobControlCapability
    + shell_capabilities::ShellOptionCapability
    + shell_capabilities::ReadCapability
{
    /// Initiates shell exit process
    fn exit_shell(&mut self);

    /// Get current GitHub status (review, mention, other)
    fn get_github_status(&self) -> (usize, usize, usize);

    /// Get current Git branch name if available
    fn get_git_branch(&self) -> Option<String>;

    /// Get number of active background jobs
    fn get_job_count(&self) -> usize;

    /// Dispatches a command to the shell's command execution system
    /// Used for commands that need to be handled by the main shell logic
    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;

    /// Typed compatibility facade for operations owned by the shell core.
    ///
    /// Existing proxy implementations continue to work through `dispatch`;
    /// the real shell overrides this to avoid a second string registry. The
    /// obvious body (delegate to `dispatch`) is not a default here - see the
    /// module doc for why this trait has no default method bodies at all.
    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()>;

    /// Saves a command output entry to the shell's history
    fn save_output_history(&mut self, entry: OutputEntry);

    /// Records a path in the shell's path history for frecency-based navigation
    fn save_path_history(&mut self, path: &str);

    /// Changes the current working directory and updates shell state
    fn changepwd(&mut self, path: &str) -> Result<()>;

    /// Returns the `pushd`/`popd` directory stack, slot 0 being the current
    /// directory. Empty when the shell has not changed directory yet.
    fn dir_stack(&self) -> Vec<String>;

    /// Replaces the directory stack wholesale.
    ///
    /// Callers are expected to route the actual directory change through
    /// [`ShellProxy::changepwd`] so path history and chpwd hooks still fire.
    fn dir_stack_set(&mut self, stack: Vec<String>);

    /// Inserts a path at the specified index in the PATH environment variable
    fn insert_path(&mut self, index: usize, path: &str);

    /// Retrieves a shell variable value by key
    fn get_var(&mut self, key: &str) -> Option<String>;

    /// Sets a shell variable (local to the shell session)
    fn set_var(&mut self, key: String, value: String);

    /// Sets an environment variable (exported to child processes)
    fn set_env_var(&mut self, key: String, value: String);

    /// Returns true when a project root has been allow-listed for `.envrc` loading.
    fn is_direnv_allowed(&self, path: &std::path::Path) -> bool;

    /// Unsets an environment variable (removes it from child processes)
    fn unset_env_var(&mut self, key: &str);

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

    /// Gets the current working directory
    fn get_current_dir(&self) -> Result<std::path::PathBuf>;

    /// Number of command history entries currently loaded in memory.
    fn command_history_len(&self) -> Option<usize>;

    /// Number of prewarmed PATH executable names currently loaded in memory.
    fn executable_cache_len(&self) -> Option<usize>;

    /// Dynamic completion cache diagnostics, when the shell runtime exposes them.
    fn completion_diagnostics(&self) -> Vec<String>;

    /// Runs shell latency probes when supported by the runtime.
    fn latency_probe_lines(&self, iterations: usize) -> Vec<String>;

    /// Retrieves a variable from the Lisp environment
    fn get_lisp_var(&self, key: &str) -> Option<String>;

    /// Current shell safety level as a typed value.
    fn safety_level(&mut self) -> SafetyLevel;

    /// Requests user confirmation for a potentially dangerous action
    fn confirm_action(&mut self, message: &str) -> Result<bool>;

    /// Checks if the current operation has been canceled (e.g. via Ctrl+C)
    fn is_canceled(&self) -> bool;

    /// Get the full output history
    fn get_full_output_history(&self) -> Vec<OutputEntry>;

    /// Clear output history and return the number of removed entries.
    fn clear_output_history(&mut self) -> usize;

    /// Get the session-local command block history.
    fn get_command_blocks(&self) -> Vec<CommandBlock>;

    /// Clear command block history and return the number of removed blocks.
    fn clear_command_blocks(&mut self) -> usize;

    /// Request that the interactive shell evaluate a command through the normal async path.
    fn request_eval_command(&mut self, command: String) -> Result<()>;

    fn capture_command(&mut self, ctx: &Context, cmd: &str) -> Result<(i32, String, String)>;

    /// Opens the external editor with the given content
    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String>;

    fn generate_command_completion_async<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String>;

    /// Ask AI for a response given a list of messages.
    fn ask_ai_async<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String>;

    /// Triggers a Lisp hook by name with arguments
    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()>;

    /// Interactive selection of an item from a list
    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>>;

    // Snippet management methods
    /// Adds a new snippet
    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool;

    /// Removes a snippet by name, returns true if it existed
    fn remove_snippet(&mut self, name: &str) -> bool;

    /// Lists all snippets
    fn list_snippets(&self) -> Vec<dsh_types::snippet::Snippet>;

    /// Gets a snippet by name
    fn get_snippet(&self, name: &str) -> Option<dsh_types::snippet::Snippet>;

    /// Updates a snippet's command and description
    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool;

    /// Records usage of a snippet
    fn record_snippet_use(&mut self, name: &str);

    // Bookmark management methods
    /// Adds a new bookmark
    fn add_bookmark(&mut self, name: String, command: String) -> bool;

    /// Removes a bookmark by name
    fn remove_bookmark(&mut self, name: &str) -> bool;

    /// Lists all bookmarks as (name, command, use_count);
    fn list_bookmarks(&self) -> Vec<(String, String, i64)>;

    /// Gets a bookmark by name (command, use_count)
    fn get_bookmark(&self, name: &str) -> Option<(String, i64)>;

    /// Records usage of a bookmark
    fn record_bookmark_use(&mut self, name: &str);

    /// Gets the last executed command from history
    fn get_last_command(&self) -> Option<String>;

    // Directory alias methods for z enhancement
    /// Adds a directory alias
    fn add_dir_alias(&mut self, name: String, path: String) -> bool;

    /// Removes a directory alias
    fn remove_dir_alias(&mut self, name: &str) -> bool;

    /// Lists all directory aliases
    fn list_dir_aliases(&self) -> Vec<(String, String)>;

    /// Gets a directory alias path by name
    fn get_dir_alias(&self, name: &str) -> Option<String>;
}

pub type ProxyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

pub(crate) fn dispatch_shell_command<P: shell_capabilities::ShellExecution + ?Sized>(
    ctx: &Context,
    proxy: &mut P,
    command: String,
) -> Result<()> {
    proxy.dispatch(ctx, "sh", vec!["-c".to_string(), command])
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

#[cfg(test)]
mod shell_proxy_tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    #[test]
    fn comp_gen_is_registered_with_an_async_handler() {
        assert!(matches!(
            get_handler("comp-gen"),
            Some(BuiltinHandler::Async { .. })
        ));
        assert!(is_builtin("comp-gen"));
        assert!(
            get_command("comp-gen").is_none(),
            "async builtins must not be exposed as synchronous command functions"
        );
    }

    #[test]
    fn confirm_action_default_denies() {
        let mut proxy = TestShellProxy::default();

        assert!(!proxy.confirm_action("dangerous?").unwrap());
        assert_eq!(proxy.safety_level(), SafetyLevel::Normal);
    }

    #[test]
    fn core_shell_actions_round_trip_through_compatibility_names() {
        let actions = [
            CoreShellAction::ChatStatus,
            CoreShellAction::Cron,
            CoreShellAction::Exit,
            CoreShellAction::History,
            CoreShellAction::Reload,
            CoreShellAction::Z,
            CoreShellAction::BlocksTui,
            CoreShellAction::BlocksPersistent,
            CoreShellAction::Jobs,
            CoreShellAction::Foreground,
            CoreShellAction::Background,
            CoreShellAction::Lisp,
            CoreShellAction::LispRun,
            CoreShellAction::Var,
            CoreShellAction::AbbrCommand,
        ];

        for action in actions {
            assert_eq!(
                CoreShellAction::from_command_name(action.command_name()),
                Some(action)
            );
        }
        assert_eq!(CoreShellAction::from_command_name("external"), None);
    }
}
