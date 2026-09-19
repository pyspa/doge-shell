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
/// The authoritative command → mode mapping lives in
/// [`crate::BUILTIN_COMMAND`]: every [`crate::BuiltinSpec`] carries its
/// `background_mode`, and the constructors require it, so registering a new
/// builtin without deciding its mode is a compile error. This accessor only
/// reads that mapping back; there is no wildcard/default policy. Unknown
/// names return `None` so re-exec consumers fail closed instead of silently
/// inheriting `Reexec`.
pub fn background_builtin_mode(name: &str) -> Option<BackgroundBuiltinMode> {
    crate::BUILTIN_COMMAND
        .get(name)
        .map(|spec| spec.background_mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BUILTIN_COMMAND, get_all_commands};

    #[test]
    fn background_builtin_accessor_covers_every_registered_command() {
        // The mapping itself is enforced at compile time (BuiltinSpec
        // requires background_mode), so this only pins the accessor
        // invariant: every registered builtin resolves to Some(mode).
        for (name, _) in get_all_commands() {
            assert!(
                background_builtin_mode(name).is_some(),
                "{name} has no background policy in BUILTIN_COMMAND"
            );
        }
        assert_eq!(
            get_all_commands().len(),
            BUILTIN_COMMAND.len(),
            "accessor must cover every registered builtin"
        );
    }

    #[test]
    fn background_builtin_mode_returns_none_for_unknown_builtin() {
        // No wildcard/default policy may exist: unknown names fail closed.
        assert_eq!(background_builtin_mode("__not_a_builtin__"), None);
    }

    #[test]
    fn background_builtin_alias_modes_match_canonical() {
        // Aliases share the handler; their background semantics must match.
        assert_eq!(
            background_builtin_mode("aic"),
            background_builtin_mode("ai-commit")
        );
        assert_eq!(
            background_builtin_mode("pm"),
            background_builtin_mode("project")
        );
        assert_eq!(
            background_builtin_mode("pj"),
            background_builtin_mode("project")
        );
    }

    #[test]
    fn background_builtin_session_bound_commands_require_parent() {
        use BackgroundBuiltinMode::ParentSessionRequired;
        // Session-bound, terminal-bound, TUI, editor, and live-runtime
        // commands must never re-exec into a fresh helper.
        for session_command in [
            "exit",
            "jobs",
            "fg",
            "bg",
            "history",
            "reload",
            "include",
            "mcp",
            "chat_prompt",
            "chat_model",
            "chat_reset",
            "chat_status",
            "skill",
            "safe-run",
            "ai-watch",
            "lisp",
            "read",
            "out",
            "__dsh_print_last_stdout",
            "blocks",
            "tm",
            "notebook-play",
            "cron",
            "snippet",
            "bookmark",
            "ga",
            "gco",
            "glog",
            "gpr",
            "gwt",
            "gh-notify",
            "ai-commit",
            "aic",
            "magit",
            "eview",
            "procs",
            "dashboard",
            "doctor",
            "timing",
            "z",
            "project",
            "pm",
            "pj",
        ] {
            assert_eq!(
                background_builtin_mode(session_command),
                Some(ParentSessionRequired),
                "{session_command} needs the live parent session"
            );
        }
    }

    #[test]
    fn background_builtin_plain_data_commands_reexec() {
        use BackgroundBuiltinMode::Reexec;
        // Pure/state-scoped builtins whose mutations stay inside the helper.
        for reexec_command in [
            "cd",
            "pushd",
            "popd",
            "dirs",
            "sched",
            "set",
            "var",
            "abbr",
            "alias",
            "export",
            "comp-gen",
            "output-gen",
            "add_path",
            "serve",
            "uuid",
            "dmv",
            "help",
            "eproject",
            "task",
            "trigger",
        ] {
            assert_eq!(
                background_builtin_mode(reexec_command),
                Some(Reexec),
                "{reexec_command} should re-exec"
            );
        }
    }
}
