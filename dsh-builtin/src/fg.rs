use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in fg command description
pub fn description() -> &'static str {
    "Resume a stopped job in the foreground"
}

/// Built-in fg (foreground) command implementation
/// Brings a background job to the foreground for interactive execution
/// Part of the shell's job control system for managing process execution
///
/// The host's status code passes through untouched: `fg` for a command
/// that exited 7 reports 7, not 0. Only a host-side infrastructure error
/// becomes exit 1. The core error carries no `fg: ` prefix; this wrapper
/// is the single diagnostic owner that adds it once.
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.foreground_job(ctx, argv) {
        Ok(code) => ExitStatus::ExitedWith(code),
        Err(err) => {
            // Report any errors that occur during job foregrounding
            ctx.write_stderr(&format!("fg: {err}")).ok();
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
    fn fg_passes_host_status_through_untouched() {
        let ctx = test_ctx();
        let mut proxy = TestShellProxy {
            foreground_status: 7,
            ..TestShellProxy::default()
        };
        let argv = vec!["fg".to_string()];

        assert_eq!(
            command(&ctx, argv, &mut proxy),
            ExitStatus::ExitedWith(7),
            "a foreground status of 7 must not be flattened to 0"
        );
    }

    #[test]
    fn fg_zero_passes_through_as_zero() {
        let ctx = test_ctx();
        let mut proxy = TestShellProxy {
            foreground_status: 0,
            ..TestShellProxy::default()
        };
        let argv = vec!["fg".to_string()];

        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(0));
    }

    #[test]
    fn fg_host_error_reports_diagnostic_and_exits_1() {
        let ctx = test_ctx();
        let mut proxy = TestShellProxy {
            foreground_error: Some("no current job".to_string()),
            ..TestShellProxy::default()
        };
        let argv = vec!["fg".to_string()];

        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(1));
    }
}
