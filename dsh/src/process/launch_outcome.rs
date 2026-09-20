//! Command failure vs infrastructure failure for process launches.
//!
//! A `Result<_, anyhow::Error>` mixes two very different things: a command
//! that failed (bad redirect, exit status 1, the shell continues) and a
//! runtime that broke (failed pipe, spawn protocol corruption, the shell
//! must abort). These types keep them apart:
//!
//! * [`CommandFailure`] — an expected shell-level failure with an exit code
//!   and a user-facing diagnostic. Never an `Err`.
//! * [`JobLaunchOutcome`] — what `Job::launch` reports: either a launched
//!   [`ProcessState`] or a [`CommandFailure`]. The surrounding
//!   `anyhow::Result` stays reserved for infrastructure failures.
//! * [`StageLaunchOutcome`] — the same split for one pipeline stage inside
//!   `Job::launch_process`.
//!
//! Scope: only redirection setup failures surface as [`CommandFailure`] for
//! now. POSIX special-builtin exit rules, `pipefail`, and `PIPESTATUS` are
//! explicitly out of scope.

use super::redirect::RedirectFailure;
use super::state::ProcessState;

/// An expected command-level failure: the command fails with `exit_code`,
/// `message` is reported on the appropriate stderr, and the shell continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFailure {
    pub exit_code: i32,
    pub message: String,
}

impl CommandFailure {
    /// Build the shared redirection-failure outcome. The `dsh: ` prefix is
    /// attached exactly once, here, so every reporter prints the same text.
    pub(crate) fn redirect(failure: &RedirectFailure) -> Self {
        Self {
            exit_code: super::redirect::REDIRECTION_FAILURE_EXIT_CODE,
            message: format!("dsh: {}", failure.message()),
        }
    }
}

/// What `Job::launch` reports.
///
/// `Process` is a launched job whose lifecycle the caller manages;
/// `CommandFailed` is an expected command failure (redirection error) that
/// the evaluator turns into a diagnostic plus a non-zero status. Only
/// internal invariant violations travel as `Err`.
#[derive(Debug)]
pub enum JobLaunchOutcome {
    Process(ProcessState),
    CommandFailed(CommandFailure),
}

/// One pipeline stage's launch split, mirroring [`JobLaunchOutcome`].
///
/// A stage that never spawned still cleans up after itself (temporary pipe
/// fds, the parent's pipe-read copy, already-spawned upstream stages)
/// before reporting `CommandFailed`.
#[derive(Debug)]
pub(crate) enum StageLaunchOutcome {
    Launched,
    CommandFailed(CommandFailure),
}
