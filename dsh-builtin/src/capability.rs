use crate::{CoreShellAction, ProxyFuture, ShellProxy};
use anyhow::Result;
use dsh_types::Context;
use dsh_types::command_block::CommandBlock;
use dsh_types::output_history::OutputEntry;
use std::path::{Path, PathBuf};

/// Environment state required by configuration-oriented builtins.
pub trait EnvironmentCapability {
    fn current_dir(&self) -> Result<PathBuf>;
    fn variable(&mut self, key: &str) -> Option<String>;
    fn set_variable(&mut self, key: String, value: String);
    fn set_exported_variable(&mut self, key: String, value: String);
    fn unset_exported_variable(&mut self, key: &str);
    fn direnv_allowed(&self, path: &Path) -> bool;
}

impl<T: ShellProxy + ?Sized> EnvironmentCapability for T {
    fn current_dir(&self) -> Result<PathBuf> {
        ShellProxy::get_current_dir(self)
    }

    fn variable(&mut self, key: &str) -> Option<String> {
        ShellProxy::get_var(self, key)
    }

    fn set_variable(&mut self, key: String, value: String) {
        ShellProxy::set_var(self, key, value);
    }

    fn set_exported_variable(&mut self, key: String, value: String) {
        ShellProxy::set_env_var(self, key, value);
    }

    fn unset_exported_variable(&mut self, key: &str) {
        ShellProxy::unset_env_var(self, key);
    }

    fn direnv_allowed(&self, path: &Path) -> bool {
        ShellProxy::is_direnv_allowed(self, path)
    }
}

/// Command execution operations used by builtins.
pub trait ExecutionCapability {
    fn dispatch_command(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;
    fn dispatch_core(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()>;
    fn change_directory(&mut self, path: &str) -> Result<()>;
    fn request_eval(&mut self, command: String) -> Result<()>;
    fn capture(&mut self, ctx: &Context, command: &str) -> Result<(i32, String, String)>;
}

impl<T: ShellProxy + ?Sized> ExecutionCapability for T {
    fn dispatch_command(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        ShellProxy::dispatch(self, ctx, cmd, argv)
    }

    fn dispatch_core(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()> {
        ShellProxy::dispatch_core_action(self, ctx, action, argv)
    }

    fn change_directory(&mut self, path: &str) -> Result<()> {
        ShellProxy::changepwd(self, path)
    }

    fn request_eval(&mut self, command: String) -> Result<()> {
        ShellProxy::request_eval_command(self, command)
    }

    fn capture(&mut self, ctx: &Context, command: &str) -> Result<(i32, String, String)> {
        ShellProxy::capture_command(self, ctx, command)
    }
}

/// Read/write history operations used by history and output builtins.
pub trait HistoryCapability {
    fn save_output(&mut self, entry: OutputEntry);
    fn output_history(&self) -> Vec<OutputEntry>;
    fn clear_outputs(&mut self) -> usize;
    fn command_blocks(&self) -> Vec<CommandBlock>;
    fn clear_blocks(&mut self) -> usize;
    fn last_command(&self) -> Option<String>;
}

impl<T: ShellProxy + ?Sized> HistoryCapability for T {
    fn save_output(&mut self, entry: OutputEntry) {
        ShellProxy::save_output_history(self, entry);
    }

    fn output_history(&self) -> Vec<OutputEntry> {
        ShellProxy::get_full_output_history(self)
    }

    fn clear_outputs(&mut self) -> usize {
        ShellProxy::clear_output_history(self)
    }

    fn command_blocks(&self) -> Vec<CommandBlock> {
        ShellProxy::get_command_blocks(self)
    }

    fn clear_blocks(&mut self) -> usize {
        ShellProxy::clear_command_blocks(self)
    }

    fn last_command(&self) -> Option<String> {
        ShellProxy::get_last_command(self)
    }
}

/// Persistent user data operations used by snippet, bookmark, and directory
/// alias builtins.
pub trait PersistenceCapability {
    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool;
    fn remove_snippet(&mut self, name: &str) -> bool;
    fn snippets(&self) -> Vec<dsh_types::snippet::Snippet>;
    fn add_bookmark(&mut self, name: String, command: String) -> bool;
    fn remove_bookmark(&mut self, name: &str) -> bool;
    fn bookmarks(&self) -> Vec<(String, String, i64)>;
    fn add_directory_alias(&mut self, name: String, path: String) -> bool;
    fn remove_directory_alias(&mut self, name: &str) -> bool;
    fn directory_aliases(&self) -> Vec<(String, String)>;
}

impl<T: ShellProxy + ?Sized> PersistenceCapability for T {
    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool {
        ShellProxy::add_snippet(self, name, command, description)
    }

    fn remove_snippet(&mut self, name: &str) -> bool {
        ShellProxy::remove_snippet(self, name)
    }

    fn snippets(&self) -> Vec<dsh_types::snippet::Snippet> {
        ShellProxy::list_snippets(self)
    }

    fn add_bookmark(&mut self, name: String, command: String) -> bool {
        ShellProxy::add_bookmark(self, name, command)
    }

    fn remove_bookmark(&mut self, name: &str) -> bool {
        ShellProxy::remove_bookmark(self, name)
    }

    fn bookmarks(&self) -> Vec<(String, String, i64)> {
        ShellProxy::list_bookmarks(self)
    }

    fn add_directory_alias(&mut self, name: String, path: String) -> bool {
        ShellProxy::add_dir_alias(self, name, path)
    }

    fn remove_directory_alias(&mut self, name: &str) -> bool {
        ShellProxy::remove_dir_alias(self, name)
    }

    fn directory_aliases(&self) -> Vec<(String, String)> {
        ShellProxy::list_dir_aliases(self)
    }
}

/// AI operations are isolated so async builtins do not need the full proxy API.
pub trait AiCapability {
    fn generate_completion<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String>;
    fn ask<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String>;
}

impl<T: ShellProxy + ?Sized> AiCapability for T {
    fn generate_completion<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String> {
        ShellProxy::generate_command_completion_async(self, command_name, help_text)
    }

    fn ask<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String> {
        ShellProxy::ask_ai_async(self, messages)
    }
}
