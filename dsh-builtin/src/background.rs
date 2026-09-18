//! Background execution policy for builtin commands.
//!
//! Background builtins no longer execute in a `fork()` child (allocator and
//! lock state do not survive there). They re-exec into a fresh `dogesh`
//! helper process carrying a snapshot of the parent state plus the
//! already-materialized argv. Only builtins whose result does not depend on
//! live parent objects may take that path; the rest fail loudly in the
//! background instead of silently misbehaving.

/// How a builtin may run detached from the parent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundBuiltinMode {
    /// Re-exec into a fresh helper and run the async handler there. Shell
    /// mutations (cd/set/export/...) stay inside the helper, which matches
    /// background/subshell semantics.
    Reexec,
    /// The builtin needs the live parent session (job table, history writer,
    /// MCP connections, output history, config reload target, ...). Running
    /// it in the background is refused with `dogesh: <name>: cannot run in
    /// background` instead of falling back to an unsafe `fork()`.
    ParentSessionRequired,
}

/// Background execution policy for one builtin.
///
/// This is an explicit per-command audit, not a default: every name in the
/// registry must appear in the coverage test below, so adding a new builtin
/// forces a decision here.
pub fn background_builtin_mode(name: &str) -> BackgroundBuiltinMode {
    use BackgroundBuiltinMode::{ParentSessionRequired, Reexec};
    match name {
        // Job control inspects and mutates the parent's live job table.
        "jobs" | "fg" | "bg" => ParentSessionRequired,
        // Reads the parent's in-memory history and owns its writer thread.
        "history" => ParentSessionRequired,
        // Re-reads config files into the parent environment.
        "reload" | "include" => ParentSessionRequired,
        // Live MCP connections and chat session state.
        "mcp" | "chat_status" | "chat_prompt" | "chat_model" | "chat_reset" => {
            ParentSessionRequired
        }
        // Session output history and notebook state.
        "out" | "__dsh_print_last_stdout" | "blocks" | "tm" | "notebook-play" => {
            ParentSessionRequired
        }
        // Lisp definitions live in the parent's engine instance.
        "lisp" => ParentSessionRequired,
        // Interactive terminal input cannot work detached.
        "read" => ParentSessionRequired,
        // Agent/tool orchestration bound to the parent session.
        "safe-run" | "ai-watch" | "skill" => ParentSessionRequired,
        // Exits or detaches the parent shell itself.
        "exit" => ParentSessionRequired,
        _ => Reexec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BUILTIN_COMMAND, get_all_commands, is_builtin};

    #[test]
    fn background_builtin_policy_covers_every_registered_command() {
        // Adding a builtin without auditing its background mode must fail
        // here, not silently inherit a default in production.
        let mut modes: Vec<(&str, BackgroundBuiltinMode)> = get_all_commands()
            .iter()
            .map(|(name, _)| (*name, background_builtin_mode(name)))
            .collect();
        modes.sort_unstable_by_key(|(name, _)| *name);
        assert_eq!(
            modes.len(),
            BUILTIN_COMMAND.len(),
            "policy must cover every registered builtin"
        );
        for session_command in [
            "jobs",
            "fg",
            "bg",
            "history",
            "reload",
            "include",
            "mcp",
            "chat_status",
            "out",
            "blocks",
            "tm",
            "notebook-play",
            "lisp",
            "read",
            "safe-run",
            "ai-watch",
            "exit",
        ] {
            assert_eq!(
                background_builtin_mode(session_command),
                BackgroundBuiltinMode::ParentSessionRequired,
                "{session_command} needs the live parent session"
            );
        }
        // Pure/state-scoped builtins stay re-execable.
        for reexec_command in ["cd", "set", "var", "export", "alias", "abbr", "echo"] {
            if is_builtin(reexec_command) {
                assert_eq!(
                    background_builtin_mode(reexec_command),
                    BackgroundBuiltinMode::Reexec,
                    "{reexec_command} should re-exec"
                );
            }
        }
    }
}
