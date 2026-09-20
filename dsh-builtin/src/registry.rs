//! Builtin command metadata, handler abstraction, and registry.
//!
//! This module owns the authoritative builtin name -> handler/description/
//! execution-policy mapping (`BUILTIN_COMMAND`) and the lookup functions
//! exposed by the crate (`get_command`, `get_handler`, `is_builtin`,
//! `get_all_commands`). Command implementations live in their dedicated
//! sibling modules; the background execution policy
//! (`BackgroundBuiltinMode`) is part of the registry metadata, not a
//! separate table.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use dsh_types::{Context, ExitStatus};

use crate::ShellProxy;
use crate::background::BackgroundBuiltinMode;
use crate::{
    abbr, add_path, ai_watch, alias, bg, blocks, bookmark, cd, chatgpt, command_timing, commit_ai,
    comp_gen, cron, dashboard, dirstack, dmv, doctor, eproject, eview, exit, exit_description,
    export, fg, ga, gco, gh_notify, glog, gpr, gwt, help, history, include, jobs, lisp, magit, mcp,
    notebook_play, out, output_gen, procs, project, read, reload, removed_sched, safe_run, serve,
    set, skill, snippet, task, tm, trigger, uuid, var, z,
};

pub type BuiltinFuture<'a> = Pin<Box<dyn Future<Output = ExitStatus> + 'a>>;

/// Immutable builtin command metadata.
///
/// `background_mode` is the authoritative background execution policy for
/// the command. It is a required constructor argument with no default, so
/// registering a new builtin without deciding its background semantics is
/// a compile error, not a silent inherit.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSpec {
    pub handler: BuiltinHandler,
    pub description: &'static str,
    pub background_mode: BackgroundBuiltinMode,
}

impl BuiltinSpec {
    pub fn new(
        func: fn(&Context, Vec<String>, &mut dyn ShellProxy) -> ExitStatus,
        description: &'static str,
        background_mode: BackgroundBuiltinMode,
    ) -> Self {
        Self {
            handler: BuiltinHandler::Sync(func),
            description,
            background_mode,
        }
    }

    pub fn new_async(
        fallback: BuiltinFn,
        run: AsyncBuiltinFn,
        description: &'static str,
        background_mode: BackgroundBuiltinMode,
    ) -> Self {
        Self {
            handler: BuiltinHandler::Async { run, fallback },
            description,
            background_mode,
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
///
/// Each entry states its background execution policy explicitly via
/// [`BuiltinSpec::background_mode`]: this table is the single authoritative
/// command → mode mapping. [`crate::background_builtin_mode`] only reads it back,
/// so there is no separate match with a wildcard default to drift.
pub static BUILTIN_COMMAND: LazyLock<HashMap<&'static str, BuiltinSpec>> = LazyLock::new(|| {
    use BackgroundBuiltinMode::{ParentSessionRequired, Reexec};
    let new = BuiltinSpec::new;
    let new_async = BuiltinSpec::new_async;
    let entries: &[(&str, BuiltinSpec)] = &[
        // Core shell commands
        ("exit", new(exit, exit_description(), ParentSessionRequired)),
        ("cd", new(cd::command, cd::description(), Reexec)),
        (
            "history",
            new(
                history::command,
                history::description(),
                ParentSessionRequired,
            ),
        ),
        // Navigation and directory management
        (
            "z",
            new(z::command, z::description(), ParentSessionRequired),
        ),
        (
            "pushd",
            new(
                dirstack::pushd_command,
                dirstack::pushd_description(),
                Reexec,
            ),
        ),
        (
            "popd",
            new(dirstack::popd_command, dirstack::popd_description(), Reexec),
        ),
        (
            "dirs",
            new(dirstack::dirs_command, dirstack::dirs_description(), Reexec),
        ),
        // Job control commands
        (
            "sched",
            new(removed_sched::command, removed_sched::description(), Reexec),
        ),
        (
            "cron",
            new(cron::command, cron::description(), ParentSessionRequired),
        ),
        (
            "jobs",
            new(jobs::command, jobs::description(), ParentSessionRequired),
        ),
        (
            "fg",
            new(fg::command, fg::description(), ParentSessionRequired),
        ),
        (
            "bg",
            new(bg::command, bg::description(), ParentSessionRequired),
        ),
        // Include command
        (
            "include",
            new(
                include::command,
                include::description(),
                ParentSessionRequired,
            ),
        ),
        // Scripting and configuration
        (
            "lisp",
            new(lisp::command, lisp::description(), ParentSessionRequired),
        ),
        ("set", new(set::command, set::description(), Reexec)),
        ("var", new(var::command, var::description(), Reexec)),
        (
            "read",
            new(read::command, read::description(), ParentSessionRequired),
        ),
        ("abbr", new(abbr::command, abbr::description(), Reexec)),
        ("alias", new(alias::command, alias::description(), Reexec)),
        (
            "export",
            new(export::command, export::description(), Reexec),
        ),
        // AI integration commands
        (
            "chat_prompt",
            new(
                chatgpt::chat_prompt,
                chatgpt::chat_prompt_description(),
                ParentSessionRequired,
            ),
        ),
        (
            "chat_model",
            new(
                chatgpt::chat_model,
                chatgpt::chat_model_description(),
                ParentSessionRequired,
            ),
        ),
        (
            "chat_reset",
            new(
                chatgpt::chat_reset,
                chatgpt::chat_reset_description(),
                ParentSessionRequired,
            ),
        ),
        (
            "chat_status",
            new(
                chatgpt::chat_status,
                chatgpt::chat_status_description(),
                ParentSessionRequired,
            ),
        ),
        (
            "skill",
            new(skill::command, skill::description(), ParentSessionRequired),
        ),
        // Safety commands
        (
            "safe-run",
            new(
                safe_run::command,
                safe_run::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "ai-watch",
            new(
                ai_watch::command,
                ai_watch::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "comp-gen",
            new_async(
                comp_gen::command,
                comp_gen::command_async,
                comp_gen::description(),
                Reexec,
            ),
        ),
        (
            "output-gen",
            new_async(
                output_gen::command,
                output_gen::command_async,
                output_gen::description(),
                Reexec,
            ),
        ),
        // Git integration commands
        (
            "ai-commit",
            new(
                commit_ai::command,
                commit_ai::description(),
                ParentSessionRequired,
            ),
        ),
        // Alias for ai-commit
        (
            "aic",
            new(
                commit_ai::command,
                commit_ai::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "glog",
            new(glog::command, glog::description(), ParentSessionRequired),
        ),
        (
            "gco",
            new(gco::command, gco::description(), ParentSessionRequired),
        ),
        (
            "ga",
            new(ga::command, ga::description(), ParentSessionRequired),
        ),
        (
            "gwt",
            new(gwt::command, gwt::description(), ParentSessionRequired),
        ),
        (
            "gh-notify",
            new(
                gh_notify::command,
                gh_notify::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "gpr",
            new(gpr::command, gpr::description(), ParentSessionRequired),
        ),
        // Utility commands
        (
            "add_path",
            new(add_path::command, add_path::description(), Reexec),
        ),
        ("serve", new(serve::command, serve::description(), Reexec)),
        ("uuid", new(uuid::command, uuid::description(), Reexec)),
        ("dmv", new(dmv::command, dmv::description(), Reexec)),
        (
            "reload",
            new(
                reload::command,
                reload::description(),
                ParentSessionRequired,
            ),
        ),
        ("help", new(help::command, help::description(), Reexec)),
        // Emacs integration commands
        (
            "eview",
            new(eview::command, eview::description(), ParentSessionRequired),
        ),
        (
            "magit",
            new(magit::command, magit::description(), ParentSessionRequired),
        ),
        (
            "eproject",
            new(eproject::command, eproject::description(), Reexec),
        ),
        // Notebook commands
        (
            "notebook-play",
            new(
                notebook_play::command,
                notebook_play::description(),
                ParentSessionRequired,
            ),
        ),
        // Performance and statistics commands
        (
            "timing",
            new(
                command_timing::command,
                command_timing::description(),
                ParentSessionRequired,
            ),
        ),
        // Output history command
        (
            "out",
            new(out::command, out::description(), ParentSessionRequired),
        ),
        (
            "tm",
            new(tm::command, tm::description(), ParentSessionRequired),
        ),
        (
            "blocks",
            new_async(
                blocks::command,
                blocks::command_async,
                blocks::description(),
                ParentSessionRequired,
            ),
        ),
        // Dashboard command
        (
            "dashboard",
            new(
                dashboard::command,
                dashboard::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "doctor",
            new(
                doctor::command,
                doctor::description(),
                ParentSessionRequired,
            ),
        ),
        // Project Management command
        (
            "procs",
            new(procs::command, procs::description(), ParentSessionRequired),
        ),
        (
            "project",
            new(
                project::command,
                project::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "pm",
            new(
                project::command,
                project::description(),
                ParentSessionRequired,
            ),
        ),
        (
            "pj",
            new(
                project::command,
                project::description(),
                ParentSessionRequired,
            ),
        ),
        // MCP management command
        (
            "mcp",
            new(mcp::command, mcp::description(), ParentSessionRequired),
        ),
        // Snippet management command
        (
            "snippet",
            new(
                snippet::command,
                snippet::description(),
                ParentSessionRequired,
            ),
        ),
        // Bookmark management command
        (
            "bookmark",
            new(
                bookmark::command,
                bookmark::description(),
                ParentSessionRequired,
            ),
        ),
        // Task runner command
        ("task", new(task::command, task::description(), Reexec)),
        // Trigger command
        (
            "trigger",
            new(trigger::command, trigger::description(), Reexec),
        ),
    ];

    let mut builtin = HashMap::with_capacity(entries.len());
    builtin.extend(entries.iter().copied());
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
