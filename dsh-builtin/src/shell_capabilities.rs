//! Narrow capability boundaries for builtin implementations.
//!
//! [`crate::ShellProxy`] remains the public compatibility facade. Every proxy
//! automatically implements these traits, while new builtin helpers and tests
//! can depend on only the operations they actually need.

use crate::{CoreShellAction, ProxyFuture, ShellProxy};
use anyhow::Result;
use dsh_types::{
    Context, command_block::CommandBlock, mcp::McpServerConfig, output_history::OutputEntry,
    safety_policy::SafetyLevel, schedule::SchedTaskSpec, schedule::SchedTaskView, snippet::Snippet,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Command execution, cancellation, confirmation, hooks, and interactive I/O.
pub trait ShellExecution {
    fn exit_shell(&mut self);
    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;
    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()>;
    fn request_eval_command(&mut self, command: String) -> Result<()>;
    fn capture_command(&mut self, ctx: &Context, cmd: &str) -> Result<(i32, String, String)>;
    fn confirm_action(&mut self, message: &str) -> Result<bool>;
    fn is_canceled(&self) -> bool;
    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()>;
    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>>;
    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String>;
}

impl<T: ShellProxy + ?Sized> ShellExecution for T {
    fn exit_shell(&mut self) {
        ShellProxy::exit_shell(self);
    }

    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        ShellProxy::dispatch(self, ctx, cmd, argv)
    }

    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()> {
        ShellProxy::dispatch_core_action(self, ctx, action, argv)
    }

    fn request_eval_command(&mut self, command: String) -> Result<()> {
        ShellProxy::request_eval_command(self, command)
    }

    fn capture_command(&mut self, ctx: &Context, cmd: &str) -> Result<(i32, String, String)> {
        ShellProxy::capture_command(self, ctx, cmd)
    }

    fn confirm_action(&mut self, message: &str) -> Result<bool> {
        ShellProxy::confirm_action(self, message)
    }

    fn is_canceled(&self) -> bool {
        ShellProxy::is_canceled(self)
    }

    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()> {
        ShellProxy::run_hook(self, hook_name, args)
    }

    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>> {
        ShellProxy::select_item(self, items)
    }

    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
        ShellProxy::open_editor(self, content, extension)
    }
}

/// Working-directory, directory-stack, frecency, and directory-alias access.
pub trait ShellNavigation {
    fn save_path_history(&mut self, path: &str);
    fn changepwd(&mut self, path: &str) -> Result<()>;
    fn dir_stack(&self) -> Vec<String>;
    fn dir_stack_set(&mut self, stack: Vec<String>);
    fn get_current_dir(&self) -> Result<PathBuf>;
    fn add_dir_alias(&mut self, name: String, path: String) -> bool;
    fn remove_dir_alias(&mut self, name: &str) -> bool;
    fn list_dir_aliases(&self) -> Vec<(String, String)>;
    fn get_dir_alias(&self, name: &str) -> Option<String>;
}

impl<T: ShellProxy + ?Sized> ShellNavigation for T {
    fn save_path_history(&mut self, path: &str) {
        ShellProxy::save_path_history(self, path);
    }

    fn changepwd(&mut self, path: &str) -> Result<()> {
        ShellProxy::changepwd(self, path)
    }

    fn dir_stack(&self) -> Vec<String> {
        ShellProxy::dir_stack(self)
    }

    fn dir_stack_set(&mut self, stack: Vec<String>) {
        ShellProxy::dir_stack_set(self, stack);
    }

    fn get_current_dir(&self) -> Result<PathBuf> {
        ShellProxy::get_current_dir(self)
    }

    fn add_dir_alias(&mut self, name: String, path: String) -> bool {
        ShellProxy::add_dir_alias(self, name, path)
    }

    fn remove_dir_alias(&mut self, name: &str) -> bool {
        ShellProxy::remove_dir_alias(self, name)
    }

    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        ShellProxy::list_dir_aliases(self)
    }

    fn get_dir_alias(&self, name: &str) -> Option<String> {
        ShellProxy::get_dir_alias(self, name)
    }
}

/// Variables, exported environment, aliases, abbreviations, and policy state.
pub trait ShellEnvironment {
    fn insert_path(&mut self, index: usize, path: &str);
    fn get_var(&mut self, key: &str) -> Option<String>;
    fn set_var(&mut self, key: String, value: String);
    fn set_env_var(&mut self, key: String, value: String);
    fn is_direnv_allowed(&self, path: &Path) -> bool;
    fn unset_env_var(&mut self, key: &str);
    fn get_alias(&mut self, name: &str) -> Option<String>;
    fn set_alias(&mut self, name: String, command: String);
    fn list_aliases(&mut self) -> HashMap<String, String>;
    fn add_abbr(&mut self, name: String, expansion: String);
    fn remove_abbr(&mut self, name: &str) -> bool;
    fn list_abbrs(&self) -> Vec<(String, String)>;
    fn get_abbr(&self, name: &str) -> Option<String>;
    fn list_execute_allowlist(&mut self) -> Vec<String>;
    fn list_exported_vars(&self) -> Vec<(String, String)>;
    fn export_var(&mut self, key: &str) -> bool;
    fn set_and_export_var(&mut self, key: String, value: String);
    fn get_lisp_var(&self, key: &str) -> Option<String>;
    fn safety_level(&mut self) -> SafetyLevel;
}

impl<T: ShellProxy + ?Sized> ShellEnvironment for T {
    fn insert_path(&mut self, index: usize, path: &str) {
        ShellProxy::insert_path(self, index, path);
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        ShellProxy::get_var(self, key)
    }

    fn set_var(&mut self, key: String, value: String) {
        ShellProxy::set_var(self, key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        ShellProxy::set_env_var(self, key, value);
    }

    fn is_direnv_allowed(&self, path: &Path) -> bool {
        ShellProxy::is_direnv_allowed(self, path)
    }

    fn unset_env_var(&mut self, key: &str) {
        ShellProxy::unset_env_var(self, key);
    }

    fn get_alias(&mut self, name: &str) -> Option<String> {
        ShellProxy::get_alias(self, name)
    }

    fn set_alias(&mut self, name: String, command: String) {
        ShellProxy::set_alias(self, name, command);
    }

    fn list_aliases(&mut self) -> HashMap<String, String> {
        ShellProxy::list_aliases(self)
    }

    fn add_abbr(&mut self, name: String, expansion: String) {
        ShellProxy::add_abbr(self, name, expansion);
    }

    fn remove_abbr(&mut self, name: &str) -> bool {
        ShellProxy::remove_abbr(self, name)
    }

    fn list_abbrs(&self) -> Vec<(String, String)> {
        ShellProxy::list_abbrs(self)
    }

    fn get_abbr(&self, name: &str) -> Option<String> {
        ShellProxy::get_abbr(self, name)
    }

    fn list_execute_allowlist(&mut self) -> Vec<String> {
        ShellProxy::list_execute_allowlist(self)
    }

    fn list_exported_vars(&self) -> Vec<(String, String)> {
        ShellProxy::list_exported_vars(self)
    }

    fn export_var(&mut self, key: &str) -> bool {
        ShellProxy::export_var(self, key)
    }

    fn set_and_export_var(&mut self, key: String, value: String) {
        ShellProxy::set_and_export_var(self, key, value);
    }

    fn get_lisp_var(&self, key: &str) -> Option<String> {
        ShellProxy::get_lisp_var(self, key)
    }

    fn safety_level(&mut self) -> SafetyLevel {
        ShellProxy::safety_level(self)
    }
}

/// Periodic task registration, control, and persistence views.
pub trait ShellScheduling {
    fn sched_add(&mut self, spec: SchedTaskSpec) -> Result<u64, String>;
    fn sched_remove(&mut self, selector: &str) -> Result<String, String>;
    fn sched_set_paused(&mut self, selector: &str, paused: bool) -> Result<String, String>;
    fn sched_trigger(&mut self, selector: &str) -> Result<String, String>;
    fn sched_list(&self) -> Vec<SchedTaskView>;
    fn sched_as_lisp(&self) -> Vec<String>;
    fn sched_enabled(&self) -> bool;
    fn sched_set_enabled(&mut self, enabled: bool);
}

impl<T: ShellProxy + ?Sized> ShellScheduling for T {
    fn sched_add(&mut self, spec: SchedTaskSpec) -> Result<u64, String> {
        ShellProxy::sched_add(self, spec)
    }

    fn sched_remove(&mut self, selector: &str) -> Result<String, String> {
        ShellProxy::sched_remove(self, selector)
    }

    fn sched_set_paused(&mut self, selector: &str, paused: bool) -> Result<String, String> {
        ShellProxy::sched_set_paused(self, selector, paused)
    }

    fn sched_trigger(&mut self, selector: &str) -> Result<String, String> {
        ShellProxy::sched_trigger(self, selector)
    }

    fn sched_list(&self) -> Vec<SchedTaskView> {
        ShellProxy::sched_list(self)
    }

    fn sched_as_lisp(&self) -> Vec<String> {
        ShellProxy::sched_as_lisp(self)
    }

    fn sched_enabled(&self) -> bool {
        ShellProxy::sched_enabled(self)
    }

    fn sched_set_enabled(&mut self, enabled: bool) {
        ShellProxy::sched_set_enabled(self, enabled);
    }
}

/// Output history, command blocks, snippets, bookmarks, and session history.
pub trait ShellSessionData {
    fn save_output_history(&mut self, entry: OutputEntry);
    fn get_full_output_history(&self) -> Vec<OutputEntry>;
    fn clear_output_history(&mut self) -> usize;
    fn get_command_blocks(&self) -> Vec<CommandBlock>;
    fn clear_command_blocks(&mut self) -> usize;
    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool;
    fn remove_snippet(&mut self, name: &str) -> bool;
    fn list_snippets(&self) -> Vec<Snippet>;
    fn get_snippet(&self, name: &str) -> Option<Snippet>;
    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool;
    fn record_snippet_use(&mut self, name: &str);
    fn add_bookmark(&mut self, name: String, command: String) -> bool;
    fn remove_bookmark(&mut self, name: &str) -> bool;
    fn list_bookmarks(&self) -> Vec<(String, String, i64)>;
    fn get_bookmark(&self, name: &str) -> Option<(String, i64)>;
    fn record_bookmark_use(&mut self, name: &str);
    fn get_last_command(&self) -> Option<String>;
    fn command_history_len(&self) -> Option<usize>;
}

impl<T: ShellProxy + ?Sized> ShellSessionData for T {
    fn save_output_history(&mut self, entry: OutputEntry) {
        ShellProxy::save_output_history(self, entry);
    }

    fn get_full_output_history(&self) -> Vec<OutputEntry> {
        ShellProxy::get_full_output_history(self)
    }

    fn clear_output_history(&mut self) -> usize {
        ShellProxy::clear_output_history(self)
    }

    fn get_command_blocks(&self) -> Vec<CommandBlock> {
        ShellProxy::get_command_blocks(self)
    }

    fn clear_command_blocks(&mut self) -> usize {
        ShellProxy::clear_command_blocks(self)
    }

    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool {
        ShellProxy::add_snippet(self, name, command, description)
    }

    fn remove_snippet(&mut self, name: &str) -> bool {
        ShellProxy::remove_snippet(self, name)
    }

    fn list_snippets(&self) -> Vec<Snippet> {
        ShellProxy::list_snippets(self)
    }

    fn get_snippet(&self, name: &str) -> Option<Snippet> {
        ShellProxy::get_snippet(self, name)
    }

    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool {
        ShellProxy::update_snippet(self, name, command, description)
    }

    fn record_snippet_use(&mut self, name: &str) {
        ShellProxy::record_snippet_use(self, name);
    }

    fn add_bookmark(&mut self, name: String, command: String) -> bool {
        ShellProxy::add_bookmark(self, name, command)
    }

    fn remove_bookmark(&mut self, name: &str) -> bool {
        ShellProxy::remove_bookmark(self, name)
    }

    fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
        ShellProxy::list_bookmarks(self)
    }

    fn get_bookmark(&self, name: &str) -> Option<(String, i64)> {
        ShellProxy::get_bookmark(self, name)
    }

    fn record_bookmark_use(&mut self, name: &str) {
        ShellProxy::record_bookmark_use(self, name);
    }

    fn get_last_command(&self) -> Option<String> {
        ShellProxy::get_last_command(self)
    }

    fn command_history_len(&self) -> Option<usize> {
        ShellProxy::command_history_len(self)
    }
}

/// Runtime status and diagnostic probes consumed by status and doctor builtins.
pub trait ShellDiagnostics {
    fn get_github_status(&self) -> (usize, usize, usize);
    fn get_git_branch(&self) -> Option<String>;
    fn get_job_count(&self) -> usize;
    fn executable_cache_len(&self) -> Option<usize>;
    fn completion_diagnostics(&self) -> Vec<String>;
    fn latency_probe_lines(&self, iterations: usize) -> Vec<String>;
}

impl<T: ShellProxy + ?Sized> ShellDiagnostics for T {
    fn get_github_status(&self) -> (usize, usize, usize) {
        ShellProxy::get_github_status(self)
    }

    fn get_git_branch(&self) -> Option<String> {
        ShellProxy::get_git_branch(self)
    }

    fn get_job_count(&self) -> usize {
        ShellProxy::get_job_count(self)
    }

    fn executable_cache_len(&self) -> Option<usize> {
        ShellProxy::executable_cache_len(self)
    }

    fn completion_diagnostics(&self) -> Vec<String> {
        ShellProxy::completion_diagnostics(self)
    }

    fn latency_probe_lines(&self, iterations: usize) -> Vec<String> {
        ShellProxy::latency_probe_lines(self, iterations)
    }
}

/// AI and MCP integration points that may perform asynchronous work.
pub trait ShellAiIntegration {
    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig>;
    fn generate_command_completion_async<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String>;
    fn ask_ai_async<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String>;
}

impl<T: ShellProxy + ?Sized> ShellAiIntegration for T {
    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
        ShellProxy::list_mcp_servers(self)
    }

    fn generate_command_completion_async<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String> {
        ShellProxy::generate_command_completion_async(self, command_name, help_text)
    }

    fn ask_ai_async<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String> {
        ShellProxy::ask_ai_async(self, messages)
    }
}

/// What the shell's safety policy says about a command the agent wants to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommandVerdict {
    /// Run it without asking.
    Allowed,
    /// Ask the user first; the string explains why.
    Confirm(String),
    /// Refuse; the string explains why.
    Denied(String),
}

/// What the user chose when asked to approve a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    /// Allow, and stop asking about this command for the rest of the session.
    AllowAlways,
    Deny,
}

/// The shell's command policy, as the chat agent's `execute` tool needs it.
///
/// Deliberately outside [`ShellProxy`]: that facade is frozen as a
/// compatibility layer, and this is a new dependency rather than an old one.
/// The agent needs three things the facade cannot express - a verdict on a full
/// command line including its pipeline, a three-way answer from the user
/// (`ShellProxy::confirm_action` only returns a bool, so "always" was
/// unreachable), and a permission set that is the agent's own rather than the
/// user's.
pub trait AgentCommandPolicy {
    /// Judge a whole command line - pipelines included - against the shell's
    /// safety guard at the current safety level.
    fn evaluate_agent_command(&mut self, command: &str) -> AgentCommandVerdict;

    /// Ask the user, offering "always".
    ///
    /// Without it a twenty-step agent run is twenty prompts, which is how a
    /// safety gate turns into a key people hold down.
    fn request_agent_approval(&mut self, message: &str) -> Result<ApprovalDecision>;

    /// Remember `command` as approved for the rest of this session.
    ///
    /// Stored and matched as the exact line the user saw, never as a prefix:
    /// approving `rm -rf target` must not also approve
    /// `rm -rf target ~/documents`.
    fn remember_agent_approval(&mut self, command: &str);

    /// Command lines approved with "always" this session, matched exactly.
    fn agent_session_approvals(&mut self) -> Vec<String>;

    /// Commands the agent may run without asking, matched by token prefix.
    ///
    /// Configured entries only - `(chat-execute-add ...)`, the JSON config and
    /// the environment variable - because those are written by a person who
    /// meant the prefix.
    fn agent_allowlist(&mut self) -> Vec<String>;
}

/// Everything a chat tool needs from its host.
///
/// A bundle rather than a new [`ShellProxy`] method, so the frozen facade stays
/// the size it is. Trait upcasting lets a `&mut dyn ChatToolHost` be passed
/// wherever a `&mut dyn ShellProxy` is expected.
pub trait ChatToolHost: ShellProxy + AgentCommandPolicy {}

impl<T: ShellProxy + AgentCommandPolicy + ?Sized> ChatToolHost for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    fn change_directory(proxy: &mut impl ShellNavigation, path: &str) -> Result<()> {
        proxy.changepwd(path)
    }

    struct NavigationFake {
        changed_to: Option<String>,
    }

    impl ShellNavigation for NavigationFake {
        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, path: &str) -> Result<()> {
            self.changed_to = Some(path.to_string());
            Ok(())
        }
        fn dir_stack(&self) -> Vec<String> {
            Vec::new()
        }
        fn dir_stack_set(&mut self, _stack: Vec<String>) {}
        fn get_current_dir(&self) -> Result<PathBuf> {
            Ok(PathBuf::from("/fake"))
        }
        fn add_dir_alias(&mut self, _name: String, _path: String) -> bool {
            false
        }
        fn remove_dir_alias(&mut self, _name: &str) -> bool {
            false
        }
        fn list_dir_aliases(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_dir_alias(&self, _name: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn legacy_shell_proxy_receives_capability_adapters() {
        let mut proxy = TestShellProxy {
            current_dir: PathBuf::from("/work"),
            allow_changepwd: true,
            ..TestShellProxy::default()
        };

        change_directory(&mut proxy, "/next").unwrap();
        assert_eq!(proxy.changed_to.as_deref(), Some("/next"));
    }

    #[test]
    fn helper_can_use_capability_only_fake() {
        let mut fake = NavigationFake { changed_to: None };

        change_directory(&mut fake, "/next").unwrap();
        assert_eq!(fake.changed_to.as_deref(), Some("/next"));
    }

    #[test]
    fn execution_capability_keeps_fail_closed_defaults() {
        let mut proxy = TestShellProxy {
            current_dir: PathBuf::from("/work"),
            ..TestShellProxy::default()
        };

        assert!(!ShellExecution::confirm_action(&mut proxy, "dangerous").unwrap());
    }
}
