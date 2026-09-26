//! The `wait` builtin: wait for background jobs and report their status.
//!
//! Operands are PIDs or `%`-prefixed job specs, with `wait -n` waiting for
//! the next completion among its targets and `wait -p VAR` publishing the
//! completed job's canonical associated PID to a shell variable
//! (`wait -n -p VAR` publishes the selected completion's PID). `wait -f`
//! remains out of scope and is rejected. Unlike `jobs`/`fg`/`bg`, `wait` cannot travel through
//! [`CoreShellAction`](super::CoreShellAction) — its result is the waited
//! child's exit status itself, not just success or failure. The handler
//! below is a thin wrapper over
//! [`JobControlCapability::wait_for_jobs`](super::shell_capabilities::JobControlCapability),
//! which the shell core implements against its live job table and
//! known-async ledger.

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in wait command description
pub fn description() -> &'static str {
    "Wait for background jobs by PID or job specification (-n for next, -p VAR for PID assignment)"
}

/// Built-in wait command implementation.
///
/// The host's status code passes through untouched: `wait` for a child that
/// exited 7 reports 7, not 0. Only a host-side infrastructure error becomes
/// exit 1 (with a diagnostic that already carries its own `wait:` prefix, so
/// this wrapper adds none).
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.wait_for_jobs(ctx, argv) {
        Ok(code) => ExitStatus::ExitedWith(code),
        Err(err) => {
            ctx.write_stderr(&format!("{err}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    fn test_ctx() -> Context {
        let pid = nix::unistd::getpid();
        Context::new_safe(pid, pid, false)
    }

    #[test]
    fn wait_passes_host_status_through_untouched() {
        let ctx = test_ctx();
        let mut proxy = TestShellProxy {
            wait_status: 7,
            ..TestShellProxy::default()
        };
        let argv = vec!["wait".to_string(), "1234".to_string()];

        assert_eq!(
            command(&ctx, argv, &mut proxy),
            ExitStatus::ExitedWith(7),
            "a child status of 7 must not be flattened to 0/1"
        );
    }

    #[test]
    fn wait_host_error_reports_diagnostic_and_exits_1() {
        let ctx = test_ctx();
        let mut proxy = TestShellProxy {
            wait_error: Some("wait: boom".to_string()),
            ..TestShellProxy::default()
        };
        let argv = vec!["wait".to_string()];

        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(1));
    }
}
