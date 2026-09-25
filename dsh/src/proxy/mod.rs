//! Shell proxy implementation for builtin command dispatch.
//!
//! This module provides the `ShellProxy` trait implementation for `Shell`,
//! routing builtin commands to their respective handlers.

mod builtin;
mod external;

mod agent_policy;
mod process_environment;
mod shell_options;
mod shell_proxy;
#[cfg(test)]
mod tests;

use crate::repl::confirmation::ConfirmationAction;
use crate::safety::SafetyResult;
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_builtin::shell_capabilities::{AgentCommandPolicy, AgentCommandVerdict, ApprovalDecision};
use dsh_builtin::{CoreShellAction, ShellProxy};
use dsh_types::{Context, mcp::McpServerConfig};
use globmatch;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Bring a stopped job back to the foreground, as the `fg` builtin does.
///
/// Exposed so the Ctrl-Z key binding can reuse the builtin's tcsetpgrp/SIGCONT
/// handling instead of reimplementing it, without widening the whole `builtin`
/// module's visibility.
pub(crate) fn resume_job_foreground(shell: &mut Shell, ctx: &Context, job_id: usize) -> Result<()> {
    builtin::jobs::execute_fg(shell, ctx, vec!["fg".to_string(), job_id.to_string()]).map(|_| ())
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_same_direnv_root(requested: &Path, allowed: &Path) -> bool {
    canonical_or_original(requested) == canonical_or_original(allowed)
}

fn read_confirmation_line(input: &mut String) -> io::Result<usize> {
    let stdin = io::stdin();
    if should_read_confirmation_from_tty(stdin.is_terminal())
        && let Ok(tty) = OpenOptions::new().read(true).open("/dev/tty")
    {
        let mut reader = BufReader::new(tty);
        return reader.read_line(input);
    }

    stdin.read_line(input)
}

fn should_read_confirmation_from_tty(stdin_is_terminal: bool) -> bool {
    stdin_is_terminal
}

fn confirmation_is_yes(input: &str) -> bool {
    input.trim().to_lowercase() == "y"
}

/// Converts the shell-side snippet record into the shape builtins see.
fn to_wire_snippet(snippet: crate::snippet::Snippet) -> dsh_types::snippet::Snippet {
    dsh_types::snippet::Snippet {
        id: snippet.id,
        name: snippet.name,
        command: snippet.command,
        description: snippet.description,
        tags: snippet.tags,
        created_at: snippet.created_at,
        last_used: snippet.last_used,
        use_count: snippet.use_count,
    }
}

// Re-export for backward compatibility
pub use builtin::jobs::parse_job_spec;
pub use builtin::reload::format_reload_error;
pub use builtin::z::parse_z_args;
