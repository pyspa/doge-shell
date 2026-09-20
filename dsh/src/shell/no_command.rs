//! No-command simple-command execution (Bash `3.7.1 Simple Command Expansion`).
//!
//! After runtime expansion leaves no command name, the shell still has work
//! to do: apply assignments to the current shell, perform redirections
//! without leaking them into later commands, and report the last command
//! substitution status (or 0 when there was none). Both the top-level
//! evaluator and the isolated helper evaluator share this function so their
//! `&&`/`||` gating cannot drift apart.

use super::substitution::ExecutionResources;
use crate::process::{CommandFailure, Redirect};
use crate::shell::Shell;
use dsh_types::Context;

/// Materialized state for one single-stage job whose expansion completed
/// normally but left no command name: assignments, redirections, the last
/// command-substitution status, and any process-substitution resources to
/// drop after execution.
#[derive(Debug)]
pub struct NoCommandMaterialization {
    pub assignments: Vec<(String, String)>,
    pub redirects: Vec<Redirect>,
    pub last_command_substitution_status: Option<i32>,
    pub resources: ExecutionResources,
}

/// Outcome of [`execute_no_command`].
///
/// `Failed` is an expected command-level failure (redirection error): the
/// caller publishes its non-zero status and continues the `;`/`&&`/`||`
/// list instead of aborting with `anyhow::Error`. The payload is the shared
/// [`CommandFailure`], so runnable and no-command paths report the same
/// status and diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoCommandExecutionResult {
    Completed(i32),
    Failed(CommandFailure),
}

/// Execute one no-command simple command:
///
/// 1. assignments land in the current shell,
/// 2. redirections apply,
/// 3. redirections restore immediately (no leak into later commands),
/// 4. the status is the last command-substitution status, or 0.
///
/// Assignment effects precede redirection: `FOO=bar > /missing/out` still
/// leaves `FOO=bar` in the shell while reporting the redirection failure.
pub fn execute_no_command(
    shell: &mut Shell,
    ctx: &mut Context,
    no_command: NoCommandMaterialization,
) -> NoCommandExecutionResult {
    if !no_command.assignments.is_empty() {
        let mut env = shell.environment.write();
        for (name, value) in &no_command.assignments {
            env.set_shell_var(name.clone(), value.clone());
        }
    }
    if !no_command.redirects.is_empty() {
        match crate::process::redirect::apply(&no_command.redirects, ctx) {
            Ok(applied) => {
                applied.restore(ctx);
                drop(applied);
            }
            Err(err) => {
                // `apply` already rolled `ctx` back on partial failure, so
                // the shell stdio is intact; report and continue the list.
                // The diagnostic prefix lives in `CommandFailure::redirect`,
                // shared with the runnable path.
                return NoCommandExecutionResult::Failed(CommandFailure::redirect(&err));
            }
        }
    }
    // `resources` (process-substitution fds/producers) drops here: fds close,
    // producers move to detached reapers, mirroring the runnable path where
    // resources drop after every stage is spawned.
    drop(no_command.resources);
    NoCommandExecutionResult::Completed(no_command.last_command_substitution_status.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shell() -> Shell {
        Shell::new(crate::environment::Environment::new())
    }

    /// Assignments land in the current shell and the last substitution
    /// status becomes the command status.
    #[test]
    fn assignments_apply_and_trace_status_is_reported() {
        let mut shell = test_shell();
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let result = execute_no_command(
            &mut shell,
            &mut ctx,
            NoCommandMaterialization {
                assignments: vec![("NO_CMD_PROBE".to_string(), "1".to_string())],
                redirects: Vec::new(),
                last_command_substitution_status: Some(7),
                resources: ExecutionResources::new(),
            },
        );
        assert_eq!(result, NoCommandExecutionResult::Completed(7));
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("NO_CMD_PROBE")
                .as_deref(),
            Some("1")
        );
    }

    /// Without substitutions the status is 0.
    #[test]
    fn plain_assignment_completes_zero() {
        let mut shell = test_shell();
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let result = execute_no_command(
            &mut shell,
            &mut ctx,
            NoCommandMaterialization {
                assignments: vec![("NO_CMD_PLAIN".to_string(), "v".to_string())],
                redirects: Vec::new(),
                last_command_substitution_status: None,
                resources: ExecutionResources::new(),
            },
        );
        assert_eq!(result, NoCommandExecutionResult::Completed(0));
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("NO_CMD_PLAIN")
                .as_deref(),
            Some("v")
        );
    }

    /// A failed redirection is a command-level failure, and the assignments
    /// are already applied: assignment → redirection ordering.
    #[test]
    fn redirect_failure_keeps_assignments_and_reports_nonzero() {
        let mut shell = test_shell();
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let missing = std::env::temp_dir().join("dsh-no-such-dir").join("out");
        let result = execute_no_command(
            &mut shell,
            &mut ctx,
            NoCommandMaterialization {
                assignments: vec![("NO_CMD_AFTER_FAIL".to_string(), "bar".to_string())],
                redirects: vec![Redirect::write(
                    nix::libc::STDOUT_FILENO,
                    missing.to_string_lossy().into_owned(),
                )],
                last_command_substitution_status: Some(7),
                resources: ExecutionResources::new(),
            },
        );
        match result {
            NoCommandExecutionResult::Failed(failure) => {
                assert_ne!(failure.exit_code, 0);
                assert!(
                    failure.message.contains("failed to create redirect file"),
                    "{:?}",
                    failure.message
                );
            }
            other => panic!("redirect failure must not complete: {other:?}"),
        }
        // The substitution status must not win over the redirect failure,
        // but the assignment stays: it ran first.
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("NO_CMD_AFTER_FAIL")
                .as_deref(),
            Some("bar")
        );
    }

    /// A successful redirection leaves no trace in the shell stdio: the
    /// context fds are exactly what they were before.
    #[test]
    fn successful_redirect_restores_context() {
        let mut shell = test_shell();
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let before = (ctx.infile, ctx.outfile, ctx.errfile);
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("no_cmd_out.txt");
        let result = execute_no_command(
            &mut shell,
            &mut ctx,
            NoCommandMaterialization {
                assignments: Vec::new(),
                redirects: vec![Redirect::write(
                    nix::libc::STDOUT_FILENO,
                    file.to_string_lossy().into_owned(),
                )],
                last_command_substitution_status: None,
                resources: ExecutionResources::new(),
            },
        );
        assert_eq!(result, NoCommandExecutionResult::Completed(0));
        assert_eq!((ctx.infile, ctx.outfile, ctx.errfile), before);
        assert!(file.exists(), "redirect side effect missing");
    }
}
