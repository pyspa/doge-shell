//! The `exit` builtin: request shell termination with a status.

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tracing::debug;

/// Status for a non-numeric `exit` operand (bash/dash use 2).
const NUMERIC_ARGUMENT_REQUIRED: i32 = 2;

/// Built-in exit command description
pub fn exit_description() -> &'static str {
    "Exit the shell"
}

/// Built-in exit command implementation.
///
/// `exit` uses the last status (`$?`), `exit N` uses `N` modulo 256. The
/// returned status becomes the command status, and the shell stops
/// evaluating once `exit_shell` has been requested. With more than one
/// operand nothing exits and the command fails, like bash.
pub fn exit(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let code = match argv.get(1..).unwrap_or_default() {
        [] => proxy
            .get_var("?")
            .and_then(|status| status.parse::<i32>().ok())
            .unwrap_or(0),
        [operand] => match operand.trim().parse::<i64>() {
            Ok(value) => value.rem_euclid(256) as i32,
            Err(_) => {
                let _ =
                    ctx.write_stderr(&format!("dsh: exit: {operand}: numeric argument required"));
                NUMERIC_ARGUMENT_REQUIRED
            }
        },
        _ => {
            let _ = ctx.write_stderr("dsh: exit: too many arguments");
            return ExitStatus::ExitedWith(1);
        }
    };
    debug!("Exit command called - initiating normal shell exit with {code}");
    proxy.exit_shell();
    ExitStatus::ExitedWith(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    fn run(argv: &[&str], proxy: &mut TestShellProxy) -> ExitStatus {
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);
        let owned: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        exit(&ctx, owned, proxy)
    }

    #[test]
    fn operand_sets_the_status_modulo_256() {
        for (operand, expected) in [("3", 3), ("0", 0), ("256", 0), ("-1", 255)] {
            let mut proxy = TestShellProxy::default();
            assert_eq!(
                run(&["exit", operand], &mut proxy),
                ExitStatus::ExitedWith(expected),
                "exit {operand}"
            );
            assert_eq!(proxy.exit_requests, 1, "exit {operand}");
        }
    }

    #[test]
    fn no_operand_uses_the_last_status() {
        let mut proxy = TestShellProxy::default();
        proxy.vars.insert("?".to_string(), "7".to_string());
        assert_eq!(run(&["exit"], &mut proxy), ExitStatus::ExitedWith(7));
        assert_eq!(proxy.exit_requests, 1);
    }

    #[test]
    fn non_numeric_operand_exits_with_two() {
        let mut proxy = TestShellProxy::default();
        assert_eq!(
            run(&["exit", "abc"], &mut proxy),
            ExitStatus::ExitedWith(NUMERIC_ARGUMENT_REQUIRED)
        );
        assert_eq!(proxy.exit_requests, 1);
    }

    #[test]
    fn too_many_operands_fail_without_exiting() {
        let mut proxy = TestShellProxy::default();
        assert_eq!(
            run(&["exit", "1", "2"], &mut proxy),
            ExitStatus::ExitedWith(1)
        );
        assert_eq!(proxy.exit_requests, 0);
    }
}
