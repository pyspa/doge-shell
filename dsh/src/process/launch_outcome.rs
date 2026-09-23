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
//! now. POSIX special-builtin exit rules and `PIPESTATUS` are explicitly
//! out of scope. (`pipefail` lives in `super::pipeline_status`, snapshotted
//! at `Job::launch`, not here.)
//!
//! [`JobLaunchContext`] is the other half of the launch boundary: the
//! caller-owned transient state `Job::launch` snapshots on entry and
//! restores on every exit path, so one job's routing never leaks into the
//! next. The durable ownership (`Job::pid`, `Job::pgid`, monitors,
//! `wait_jobs`) stays on the job itself — `ctx.pgid` is temporary routing
//! state, `job.pgid` is the last-man-standing ownership record.

use super::redirect::RedirectFailure;
use super::state::ProcessState;
use dsh_types::Context;
use nix::unistd::Pid;
use std::os::unix::io::RawFd;

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

/// Snapshot of the caller-owned transient state that `Job::launch`
/// temporarily mutates. Restored on every exit path so stale state never
/// leaks to the next job or helper boundary.
///
/// Only job-local routing is captured: `captured_out`/`output_observer`
/// (the capture/helper protocol contract) and the session state
/// (`interactive`, `save_history`, shell ids, terminal state) stay with
/// the caller. Restoration returns to the *entry value*, never to a
/// default — a helper's `Some(helper_pid)` group anchor survives the inner
/// launch exactly as a top-level `None` does.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JobLaunchContext {
    foreground: bool,
    infile: RawFd,
    outfile: RawFd,
    errfile: RawFd,
    pid: Option<Pid>,
    pgid: Option<Pid>,
    process_count: u32,
}

impl JobLaunchContext {
    pub(crate) fn capture(ctx: &Context) -> Self {
        Self {
            foreground: ctx.foreground,
            infile: ctx.infile,
            outfile: ctx.outfile,
            errfile: ctx.errfile,
            pid: ctx.pid,
            pgid: ctx.pgid,
            process_count: ctx.process_count,
        }
    }

    pub(crate) fn restore(self, ctx: &mut Context) {
        ctx.foreground = self.foreground;
        ctx.infile = self.infile;
        ctx.outfile = self.outfile;
        ctx.errfile = self.errfile;
        ctx.pid = self.pid;
        ctx.pgid = self.pgid;
        ctx.process_count = self.process_count;
    }
}
