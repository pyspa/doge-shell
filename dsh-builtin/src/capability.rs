//! Legacy capability traits, kept only while `ExecutionCapability` and
//! `AiCapability` still have consumers (16 files, `rg -l "capability::"`).
//!
//! `EnvironmentCapability`, `HistoryCapability`, and `PersistenceCapability`
//! used to live here too; they were deleted because they had zero consumers
//! and duplicated [`crate::shell_capabilities`] method names
//! (`add_snippet`/`remove_snippet`/`add_bookmark`/`remove_bookmark` collided
//! with `ShellSessionData`). A file that `use`d both traits would have failed
//! to compile with an ambiguous-method error the moment it needed one method
//! from each side.
//!
//! Do not add new traits here. A new builtin dependency belongs in
//! [`crate::shell_capabilities`] - either as a method on one of the six
//! traits that mirror [`ShellProxy`] one-to-one, or, if it has no equivalent
//! on `ShellProxy`, as its own standalone trait implemented directly per host
//! type (the pattern `AgentCommandPolicy` already uses). See
//! `docs/ai/skills/doge-shell-repo/references/invariants.md` under
//! "二重化しているもの".

use crate::{CoreShellAction, ProxyFuture, ShellProxy};
use anyhow::Result;
use dsh_types::Context;

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
