use anyhow::Result;
use dsh_types::{
    Context, ExitStatus, command_block::CommandBlock, mcp::McpServerConfig,
    output_history::OutputEntry, safety_policy::SafetyLevel,
};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use tracing::debug;

// Builtin command modules
mod abbr;
mod add_path;
mod ai_watch;

mod alias;
mod bg;
mod blocks;
pub mod cd;
mod chatgpt;
mod dashboard;
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
pub use chatgpt::execute_chat_message;
pub use chatgpt::{McpConnectionStatus, McpManager, McpRuntimeStateSnapshot, McpServerStatus};
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
pub mod procs;
pub mod project;
pub mod project_context;
mod read;
pub mod sched;
pub mod shell_capabilities;
#[cfg(test)]
pub(crate) mod test_support;

mod reload;
pub mod serve;
mod set;
mod skim_runner;
mod snippet;
pub mod task;
pub mod tm;
mod trigger;
mod uuid;
mod var;
mod z;

/// Shell-owned operations that cannot be implemented inside `dsh-builtin`.
///
/// Public builtin names remain strings at the CLI boundary, but the handoff to
/// the shell core is exhaustive and typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreShellAction {
    Exit,
    History,
    Reload,
    Z,
    BlocksTui,
    Jobs,
    Foreground,
    Background,
    Lisp,
    LispRun,
    Var,
    Read,
}

impl CoreShellAction {
    pub const fn command_name(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::History => "history",
            Self::Reload => "reload",
            Self::Z => "z",
            Self::BlocksTui => "blocks-tui",
            Self::Jobs => "jobs",
            Self::Foreground => "fg",
            Self::Background => "bg",
            Self::Lisp => "lisp",
            Self::LispRun => "lisp-run",
            Self::Var => "var",
            Self::Read => "read",
        }
    }

    pub fn from_command_name(command: &str) -> Option<Self> {
        Some(match command {
            "exit" => Self::Exit,
            "history" => Self::History,
            "reload" => Self::Reload,
            "z" => Self::Z,
            "blocks-tui" => Self::BlocksTui,
            "jobs" => Self::Jobs,
            "fg" => Self::Foreground,
            "bg" => Self::Background,
            "lisp" => Self::Lisp,
            "lisp-run" => Self::LispRun,
            "var" => Self::Var,
            "read" => Self::Read,
            _ => return None,
        })
    }
}

/// Trait that provides an interface for builtin commands to interact with the shell
/// This allows builtin commands to perform shell operations without direct coupling
pub trait ShellProxy {
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
    /// the real shell overrides this to avoid a second string registry.
    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()> {
        self.dispatch(ctx, action.command_name(), argv)
    }

    /// Saves a command output entry to the shell's history
    fn save_output_history(&mut self, _entry: OutputEntry) {}

    /// Records a path in the shell's path history for frecency-based navigation
    fn save_path_history(&mut self, path: &str);

    /// Changes the current working directory and updates shell state
    fn changepwd(&mut self, path: &str) -> Result<()>;

    /// Returns the `pushd`/`popd` directory stack, slot 0 being the current
    /// directory. Empty when the shell has not changed directory yet.
    ///
    /// Defaulted so the many test doubles of this trait keep compiling.
    fn dir_stack(&self) -> Vec<String> {
        Vec::new()
    }

    /// Replaces the directory stack wholesale.
    ///
    /// Callers are expected to route the actual directory change through
    /// [`ShellProxy::changepwd`] so path history and chpwd hooks still fire.
    fn dir_stack_set(&mut self, _stack: Vec<String>) {}

    /// Registers a periodic task, returning its id.
    ///
    /// All the scheduler hooks below are defaulted for the same reason as the
    /// directory-stack pair: this trait has a large number of test doubles.
    fn sched_add(&mut self, _spec: dsh_types::schedule::SchedTaskSpec) -> Result<u64, String> {
        Err("scheduler unavailable".to_string())
    }

    fn sched_remove(&mut self, _selector: &str) -> Result<String, String> {
        Err("scheduler unavailable".to_string())
    }

    /// Pauses or resumes one task.
    fn sched_set_paused(&mut self, _selector: &str, _paused: bool) -> Result<String, String> {
        Err("scheduler unavailable".to_string())
    }

    /// Makes a task due on the next scan.
    fn sched_trigger(&mut self, _selector: &str) -> Result<String, String> {
        Err("scheduler unavailable".to_string())
    }

    fn sched_list(&self) -> Vec<dsh_types::schedule::SchedTaskView> {
        Vec::new()
    }

    /// `sched list --lisp`: the `sched-add` calls that recreate the task set.
    fn sched_as_lisp(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether the scheduler as a whole is running.
    fn sched_enabled(&self) -> bool {
        false
    }

    fn sched_set_enabled(&mut self, _enabled: bool) {}

    /// Inserts a path at the specified index in the PATH environment variable
    fn insert_path(&mut self, index: usize, path: &str);

    /// Retrieves a shell variable value by key
    fn get_var(&mut self, key: &str) -> Option<String>;

    /// Sets a shell variable (local to the shell session)
    fn set_var(&mut self, key: String, value: String);

    /// Sets an environment variable (exported to child processes)
    fn set_env_var(&mut self, key: String, value: String);

    /// Returns true when a project root has been allow-listed for `.envrc` loading.
    fn is_direnv_allowed(&self, _path: &std::path::Path) -> bool {
        false
    }

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
    fn command_history_len(&self) -> Option<usize> {
        None
    }

    /// Number of prewarmed PATH executable names currently loaded in memory.
    fn executable_cache_len(&self) -> Option<usize> {
        None
    }

    /// Dynamic completion cache diagnostics, when the shell runtime exposes them.
    fn completion_diagnostics(&self) -> Vec<String> {
        Vec::new()
    }

    /// Runs shell latency probes when supported by the runtime.
    fn latency_probe_lines(&self, _iterations: usize) -> Vec<String> {
        Vec::new()
    }

    /// Retrieves a variable from the Lisp environment
    fn get_lisp_var(&self, key: &str) -> Option<String>;

    /// Current shell safety level as a typed value.
    fn safety_level(&mut self) -> SafetyLevel {
        SafetyLevel::from_env_value(self.get_var("SAFETY_LEVEL"))
    }

    /// Requests user confirmation for a potentially dangerous action
    fn confirm_action(&mut self, _message: &str) -> Result<bool> {
        Ok(false)
    }

    /// Checks if the current operation has been canceled (e.g. via Ctrl+C)
    fn is_canceled(&self) -> bool {
        false
    }

    /// Get the full output history
    fn get_full_output_history(&self) -> Vec<OutputEntry> {
        Vec::new()
    }

    /// Clear output history and return the number of removed entries.
    fn clear_output_history(&mut self) -> usize {
        0
    }

    /// Get the session-local command block history.
    fn get_command_blocks(&self) -> Vec<CommandBlock> {
        Vec::new()
    }

    /// Clear command block history and return the number of removed blocks.
    fn clear_command_blocks(&mut self) -> usize {
        0
    }

    /// Request that the interactive shell evaluate a command through the normal async path.
    fn request_eval_command(&mut self, _command: String) -> Result<()> {
        Err(anyhow::anyhow!("request_eval_command not implemented"))
    }

    fn capture_command(&mut self, _ctx: &Context, _cmd: &str) -> Result<(i32, String, String)> {
        // Default implementation returns error as this requires direct shell access
        Err(anyhow::anyhow!("capture_command not implemented"))
    }

    /// Opens the external editor with the given content
    fn open_editor(&mut self, _content: &str, _extension: &str) -> Result<String> {
        Err(anyhow::anyhow!("open_editor not implemented"))
    }

    fn generate_command_completion_async<'a>(
        &'a mut self,
        _command_name: &'a str,
        _help_text: &'a str,
    ) -> ProxyFuture<'a, String> {
        Box::pin(async move {
            Err(anyhow::anyhow!(
                "generate_command_completion_async not implemented"
            ))
        })
    }

    /// Ask AI for a response given a list of messages.
    fn ask_ai_async<'a>(
        &'a mut self,
        _messages: Vec<serde_json::Value>,
    ) -> ProxyFuture<'a, String> {
        Box::pin(async move { Err(anyhow::anyhow!("ask_ai_async not implemented")) })
    }

    /// Triggers a Lisp hook by name with arguments
    fn run_hook(&mut self, _hook_name: &str, _args: Vec<String>) -> Result<()> {
        Err(anyhow::anyhow!("run_hook not implemented"))
    }

    /// Interactive selection of an item from a list
    fn select_item(&mut self, _items: Vec<String>) -> Result<Option<String>> {
        Err(anyhow::anyhow!("select_item not implemented"))
    }

    // Snippet management methods
    /// Adds a new snippet
    fn add_snippet(
        &mut self,
        _name: String,
        _command: String,
        _description: Option<String>,
    ) -> bool {
        false
    }

    /// Removes a snippet by name, returns true if it existed
    fn remove_snippet(&mut self, _name: &str) -> bool {
        false
    }

    /// Lists all snippets
    fn list_snippets(&self) -> Vec<dsh_types::snippet::Snippet> {
        Vec::new()
    }

    /// Gets a snippet by name
    fn get_snippet(&self, _name: &str) -> Option<dsh_types::snippet::Snippet> {
        None
    }

    /// Updates a snippet's command and description
    fn update_snippet(&mut self, _name: &str, _command: &str, _description: Option<&str>) -> bool {
        false
    }

    /// Records usage of a snippet
    fn record_snippet_use(&mut self, _name: &str) {}

    // Bookmark management methods
    /// Adds a new bookmark
    fn add_bookmark(&mut self, _name: String, _command: String) -> bool {
        false
    }

    /// Removes a bookmark by name
    fn remove_bookmark(&mut self, _name: &str) -> bool {
        false
    }

    /// Lists all bookmarks as (name, command, use_count);
    fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
        Vec::new()
    }

    /// Gets a bookmark by name (command, use_count)
    fn get_bookmark(&self, _name: &str) -> Option<(String, i64)> {
        None
    }

    /// Records usage of a bookmark
    fn record_bookmark_use(&mut self, _name: &str) {}

    /// Gets the last executed command from history
    fn get_last_command(&self) -> Option<String> {
        None
    }

    // Directory alias methods for z enhancement
    /// Adds a directory alias
    fn add_dir_alias(&mut self, _name: String, _path: String) -> bool {
        false
    }

    /// Removes a directory alias
    fn remove_dir_alias(&mut self, _name: &str) -> bool {
        false
    }

    /// Lists all directory aliases
    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Gets a directory alias path by name
    fn get_dir_alias(&self, _name: &str) -> Option<String> {
        None
    }
}

pub type ProxyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;
pub type BuiltinFuture<'a> = Pin<Box<dyn Future<Output = ExitStatus> + 'a>>;

pub(crate) fn dispatch_shell_command<P: shell_capabilities::ShellExecution + ?Sized>(
    ctx: &Context,
    proxy: &mut P,
    command: String,
) -> Result<()> {
    proxy.dispatch(ctx, "sh", vec!["-c".to_string(), command])
}

/// Immutable builtin command metadata.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSpec {
    pub handler: BuiltinHandler,
    pub description: &'static str,
}

impl BuiltinSpec {
    pub fn new(
        func: fn(&Context, Vec<String>, &mut dyn ShellProxy) -> ExitStatus,
        description: &'static str,
    ) -> Self {
        Self {
            handler: BuiltinHandler::Sync(func),
            description,
        }
    }

    pub fn new_async(fallback: BuiltinFn, run: AsyncBuiltinFn, description: &'static str) -> Self {
        Self {
            handler: BuiltinHandler::Async { run, fallback },
            description,
        }
    }
}

/// Type alias for the builtin command function type to reduce complexity.
pub type BuiltinFn = fn(&Context, Vec<String>, &mut dyn ShellProxy) -> ExitStatus;
pub type AsyncBuiltinFn =
    for<'a> fn(&'a Context, Vec<String>, &'a mut dyn ShellProxy) -> BuiltinFuture<'a>;

#[derive(Clone, Copy)]
pub enum BuiltinHandler {
    Sync(BuiltinFn),
    Async {
        run: AsyncBuiltinFn,
        fallback: BuiltinFn,
    },
}

impl std::fmt::Debug for BuiltinHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sync(_) => formatter.write_str("BuiltinHandler::Sync"),
            Self::Async { .. } => formatter.write_str("BuiltinHandler::Async"),
        }
    }
}

impl BuiltinHandler {
    pub async fn execute(
        self,
        ctx: &Context,
        argv: Vec<String>,
        proxy: &mut dyn ShellProxy,
    ) -> ExitStatus {
        match self {
            Self::Sync(run) => run(ctx, argv, proxy),
            Self::Async { run, .. } => run(ctx, argv, proxy).await,
        }
    }

    pub fn execute_sync(
        self,
        ctx: &Context,
        argv: Vec<String>,
        proxy: &mut dyn ShellProxy,
    ) -> ExitStatus {
        match self {
            Self::Sync(run) | Self::Async { fallback: run, .. } => run(ctx, argv, proxy),
        }
    }
}

impl BuiltinSpec {
    pub fn execute_sync(
        &self,
        ctx: &Context,
        argv: Vec<String>,
        proxy: &mut dyn ShellProxy,
    ) -> ExitStatus {
        self.handler.execute_sync(ctx, argv, proxy)
    }
}

/// Immutable registry of all builtin commands.
pub static BUILTIN_COMMAND: LazyLock<HashMap<&'static str, BuiltinSpec>> = LazyLock::new(|| {
    let mut builtin = HashMap::new();

    // Core shell commands
    builtin.insert("exit", BuiltinSpec::new(exit, exit_description()));
    builtin.insert("cd", BuiltinSpec::new(cd::command, cd::description()));
    builtin.insert(
        "history",
        BuiltinSpec::new(history::command, history::description()),
    );

    // Navigation and directory management
    builtin.insert("z", BuiltinSpec::new(z::command, z::description()));
    builtin.insert(
        "pushd",
        BuiltinSpec::new(dirstack::pushd_command, dirstack::pushd_description()),
    );
    builtin.insert(
        "popd",
        BuiltinSpec::new(dirstack::popd_command, dirstack::popd_description()),
    );
    builtin.insert(
        "dirs",
        BuiltinSpec::new(dirstack::dirs_command, dirstack::dirs_description()),
    );

    // Job control commands
    builtin.insert(
        "sched",
        BuiltinSpec::new(sched::command, sched::description()),
    );
    builtin.insert("jobs", BuiltinSpec::new(jobs::command, jobs::description()));
    builtin.insert("fg", BuiltinSpec::new(fg::command, fg::description()));
    builtin.insert("bg", BuiltinSpec::new(bg::command, bg::description()));

    // Include command
    builtin.insert(
        "include",
        BuiltinSpec::new(include::command, include::description()),
    );
    // Scripting and configuration
    builtin.insert("lisp", BuiltinSpec::new(lisp::command, lisp::description()));
    builtin.insert("set", BuiltinSpec::new(set::command, set::description()));
    builtin.insert("var", BuiltinSpec::new(var::command, var::description()));
    builtin.insert("read", BuiltinSpec::new(read::command, read::description()));
    builtin.insert("abbr", BuiltinSpec::new(abbr::command, abbr::description()));
    builtin.insert(
        "alias",
        BuiltinSpec::new(alias::command, alias::description()),
    );
    builtin.insert(
        "export",
        BuiltinSpec::new(export::command, export::description()),
    );

    // AI integration commands

    builtin.insert(
        "chat_prompt",
        BuiltinSpec::new(chatgpt::chat_prompt, chatgpt::chat_prompt_description()),
    );
    builtin.insert(
        "chat_model",
        BuiltinSpec::new(chatgpt::chat_model, chatgpt::chat_model_description()),
    );

    // Safety commands
    builtin.insert(
        "safe-run",
        BuiltinSpec::new(safe_run::command, safe_run::description()),
    );
    builtin.insert(
        "ai-watch",
        BuiltinSpec::new(ai_watch::command, ai_watch::description()),
    );

    builtin.insert(
        "comp-gen",
        BuiltinSpec::new_async(
            comp_gen::command,
            comp_gen::command_async,
            comp_gen::description(),
        ),
    );

    // Git integration commands
    builtin.insert(
        "ai-commit",
        BuiltinSpec::new(commit_ai::command, commit_ai::description()),
    );
    // Alias for ai-commit
    builtin.insert(
        "aic",
        BuiltinSpec::new(commit_ai::command, commit_ai::description()),
    );

    builtin.insert("glog", BuiltinSpec::new(glog::command, glog::description()));
    builtin.insert("gco", BuiltinSpec::new(gco::command, gco::description()));
    builtin.insert("ga", BuiltinSpec::new(ga::command, ga::description()));
    builtin.insert("gwt", BuiltinSpec::new(gwt::command, gwt::description()));
    builtin.insert(
        "gh-notify",
        BuiltinSpec::new(gh_notify::command, gh_notify::description()),
    );
    builtin.insert("gpr", BuiltinSpec::new(gpr::command, gpr::description()));

    // Utility commands
    builtin.insert(
        "add_path",
        BuiltinSpec::new(add_path::command, add_path::description()),
    );
    builtin.insert(
        "serve",
        BuiltinSpec::new(serve::command, serve::description()),
    );
    builtin.insert("uuid", BuiltinSpec::new(uuid::command, uuid::description()));
    builtin.insert("dmv", BuiltinSpec::new(dmv::command, dmv::description()));
    builtin.insert(
        "reload",
        BuiltinSpec::new(reload::command, reload::description()),
    );
    builtin.insert("help", BuiltinSpec::new(help::command, help::description()));

    // Emacs integration commands
    builtin.insert(
        "eview",
        BuiltinSpec::new(eview::command, eview::description()),
    );
    builtin.insert(
        "magit",
        BuiltinSpec::new(magit::command, magit::description()),
    );
    builtin.insert(
        "eproject",
        BuiltinSpec::new(eproject::command, eproject::description()),
    );

    // Notebook commands
    builtin.insert(
        "notebook-play",
        BuiltinSpec::new(notebook_play::command, notebook_play::description()),
    );

    // Performance and statistics commands
    builtin.insert(
        "timing",
        BuiltinSpec::new(command_timing::command, command_timing::description()),
    );

    // Output history command
    builtin.insert("out", BuiltinSpec::new(out::command, out::description()));
    builtin.insert(
        "__dsh_print_last_stdout",
        BuiltinSpec::new(out::print_last_stdout, out::print_last_stdout_description()),
    );

    builtin.insert("tm", BuiltinSpec::new(tm::command, tm::description()));
    builtin.insert(
        "blocks",
        BuiltinSpec::new_async(
            blocks::command,
            blocks::command_async,
            blocks::description(),
        ),
    );

    // Dashboard command
    builtin.insert(
        "dashboard",
        BuiltinSpec::new(dashboard::command, dashboard::description()),
    );
    builtin.insert(
        "doctor",
        BuiltinSpec::new(doctor::command, doctor::description()),
    );

    // Project Management command
    builtin.insert(
        "procs",
        BuiltinSpec::new(procs::command, procs::description()),
    );

    builtin.insert(
        "project",
        BuiltinSpec::new(project::command, project::description()),
    );
    builtin.insert(
        "pm",
        BuiltinSpec::new(project::command, project::description()),
    );
    builtin.insert(
        "pj",
        BuiltinSpec::new(project::command, project::description()),
    );

    // MCP management command
    builtin.insert("mcp", BuiltinSpec::new(mcp::command, mcp::description()));

    // Snippet management command
    builtin.insert(
        "snippet",
        BuiltinSpec::new(snippet::command, snippet::description()),
    );

    // Bookmark management command
    builtin.insert(
        "bookmark",
        BuiltinSpec::new(bookmark::command, bookmark::description()),
    );

    // Task runner command
    builtin.insert("task", BuiltinSpec::new(task::command, task::description()));

    // Trigger command
    builtin.insert(
        "trigger",
        BuiltinSpec::new(trigger::command, trigger::description()),
    );

    builtin
});

/// Retrieves an inherently synchronous builtin command by name.
///
/// Async handlers deliberately return `None`; callers that execute arbitrary
/// builtins must use [`get_handler`] so they cannot accidentally bypass the
/// async implementation through its fork-only fallback.
pub fn get_command(name: &str) -> Option<BuiltinFn> {
    BUILTIN_COMMAND
        .get(name)
        .and_then(|spec| match spec.handler {
            BuiltinHandler::Sync(run) => Some(run),
            BuiltinHandler::Async { .. } => None,
        })
}

pub fn get_handler(name: &str) -> Option<BuiltinHandler> {
    BUILTIN_COMMAND.get(name).map(|spec| spec.handler)
}

pub fn is_builtin(name: &str) -> bool {
    BUILTIN_COMMAND.contains_key(name)
}

/// Get all builtin commands with their descriptions
pub fn get_all_commands() -> Vec<(&'static str, &'static str)> {
    let mut commands = BUILTIN_COMMAND
        .iter()
        .map(|(name, spec)| (*name, spec.description))
        .collect::<Vec<_>>();
    commands.sort_unstable_by_key(|(name, _)| *name);
    commands
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
            CoreShellAction::Exit,
            CoreShellAction::History,
            CoreShellAction::Reload,
            CoreShellAction::Z,
            CoreShellAction::BlocksTui,
            CoreShellAction::Jobs,
            CoreShellAction::Foreground,
            CoreShellAction::Background,
            CoreShellAction::Lisp,
            CoreShellAction::LispRun,
            CoreShellAction::Var,
            CoreShellAction::Read,
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
