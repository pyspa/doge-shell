use super::ShellProxy;
use dsh_types::shell_options::ShellOption;
use dsh_types::{Context, ExitStatus};

/// Built-in set command description
pub fn description() -> &'static str {
    "Set shell options"
}

/// Prints usage information for the set command
fn print_usage(ctx: &Context, cmd_name: &str) {
    let _ = ctx.write_stdout(&format!(
        "Usage: {cmd_name} [-o|+o] [NAME]\n       {cmd_name} KEY VALUE\n       {cmd_name} -x KEY VALUE"
    ));
}

/// Built-in set command implementation.
///
/// Canonical option-control forms:
///
/// * `set -o pipefail` enables, `set +o pipefail` disables.
/// * `set -o` prints current values (`pipefail on|off`).
/// * `set +o` prints re-inputable commands (`set -o pipefail`).
///
/// Legacy compatibility (kept so existing scripts keep working):
///
/// * `set KEY VALUE` sets a local shell variable.
/// * `set -x KEY VALUE` (or `set --export KEY VALUE`) sets an exported variable.
///
/// Every other `-X`/`+X` form (including bare `set -x`, `set -e`, `set -u`,
/// `set --`, positional parameters) is rejected with a non-zero status and
/// a diagnostic. In particular `set -x` alone must never pretend to be a
/// successful no-op: xtrace is not implemented.
pub fn command(ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let cmd_name = args.first().cloned().unwrap_or_else(|| "set".to_string());
    let operands: &[String] = args.get(1..).unwrap_or(&[]);

    // `-h` / `--help` anywhere keeps the historical usage exit status.
    if operands.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_usage(ctx, &cmd_name);
        return ExitStatus::ExitedWith(0);
    }

    if let Some((flag, rest)) = operands.split_first() {
        if *flag == "-o" || *flag == "+o" {
            return handle_option_control(ctx, proxy, &cmd_name, *flag == "-o", rest);
        }
        if *flag == "-x" || *flag == "--export" {
            // Legacy exported-variable setter needs exactly KEY VALUE.
            // A bare `set -x` is an unsupported option, not a silent success.
            if rest.len() == 2 {
                proxy.set_env_var(rest[0].clone(), rest[1].clone());
                return ExitStatus::ExitedWith(0);
            }
            let _ = ctx.write_stderr(&format!(
                "{cmd_name}: -x: unsupported option (xtrace is not implemented; use `-x KEY VALUE` for the legacy export setter)"
            ));
            return ExitStatus::ExitedWith(1);
        }
        // Any other leading dash/plus is an unsupported `set` form.
        if flag.starts_with('-') || flag.starts_with('+') {
            let _ = ctx.write_stderr(&format!("{cmd_name}: {flag}: unsupported option"));
            return ExitStatus::ExitedWith(1);
        }
    }

    // Legacy local-variable setter: exactly KEY VALUE, no option prefix.
    if operands.len() == 2 {
        proxy.set_var(operands[0].clone(), operands[1].clone());
        return ExitStatus::ExitedWith(0);
    }

    // Historical fallback for arity mismatches that do not look like
    // option control (e.g. bare `set`, `set FOO`).
    print_usage(ctx, &cmd_name);
    ExitStatus::ExitedWith(0)
}

fn handle_option_control(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    cmd_name: &str,
    enable_form: bool,
    rest: &[String],
) -> ExitStatus {
    match rest {
        [] => {
            if enable_form {
                for option in ShellOption::ALL {
                    let state = if proxy.shell_option_enabled(option) {
                        "on"
                    } else {
                        "off"
                    };
                    let _ = ctx.write_stdout(&format!("{} {state}", option.name()));
                }
            } else {
                for option in ShellOption::ALL {
                    if proxy.shell_option_enabled(option) {
                        let _ = ctx.write_stdout(&format!("set -o {}", option.name()));
                    } else {
                        let _ = ctx.write_stdout(&format!("set +o {}", option.name()));
                    }
                }
            }
            ExitStatus::ExitedWith(0)
        }
        [name] => match ShellOption::parse(name) {
            Some(option) => {
                proxy.set_shell_option(option, enable_form);
                ExitStatus::ExitedWith(0)
            }
            None => {
                let _ = ctx.write_stderr(&format!("{cmd_name}: {name}: unknown option"));
                ExitStatus::ExitedWith(1)
            }
        },
        _ => {
            let _ = ctx.write_stderr(&format!("{cmd_name}: too many arguments"));
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use std::io::Read as _;
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};

    fn test_ctx() -> Context {
        let pid = nix::unistd::getpid();
        Context::new_safe(pid, pid, false)
    }

    fn null_ctx() -> Context {
        let mut ctx = test_ctx();
        let null_out = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        let null_err = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        ctx.outfile = null_out;
        ctx.errfile = null_err;
        ctx
    }

    /// Close a `null_ctx`'s redirected fds. `Context` owns raw fds without
    /// `Drop`, so tests must close explicitly. Only call on contexts whose
    /// `outfile`/`errfile` were redirected above — never on a default
    /// context still naming stdout/stderr.
    fn close_null_ctx(ctx: &Context) {
        let _ = nix::unistd::close(ctx.outfile);
        let _ = nix::unistd::close(ctx.errfile);
    }

    fn run_with_captured_stdout(
        argv: Vec<String>,
        proxy: &mut TestShellProxy,
    ) -> (ExitStatus, String) {
        let mut ctx = test_ctx();
        let null_err = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        ctx.errfile = null_err;
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let read_fd = read.into_raw_fd();
        let write_fd = write.into_raw_fd();
        ctx.outfile = write_fd;
        let status = command(&ctx, argv, proxy);
        // Close the write end so the reader sees EOF. `write_stdout`
        // forgets its `File`, so this is the only close.
        let _ = nix::unistd::close(write_fd);
        let mut output = String::new();
        let mut read_file = unsafe { std::fs::File::from_raw_fd(read_fd) };
        read_file.read_to_string(&mut output).expect("read stdout");
        let _ = nix::unistd::close(null_err);
        (status, output)
    }

    #[test]
    fn set_o_pipefail_enables() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        let argv = vec!["set".to_string(), "-o".to_string(), "pipefail".to_string()];
        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(0));
        assert!(proxy.shell_options.enabled(ShellOption::Pipefail));
        close_null_ctx(&ctx);
    }

    #[test]
    fn set_plus_o_pipefail_disables() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        proxy.shell_options.set(ShellOption::Pipefail, true);
        let argv = vec!["set".to_string(), "+o".to_string(), "pipefail".to_string()];
        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(0));
        assert!(!proxy.shell_options.enabled(ShellOption::Pipefail));
        close_null_ctx(&ctx);
    }

    #[test]
    fn set_o_lists_current_state() {
        let mut proxy = TestShellProxy::default();
        let argv = vec!["set".to_string(), "-o".to_string()];
        let (status, output) = run_with_captured_stdout(argv, &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(output.contains("pipefail off"), "got: {output:?}");

        proxy.shell_options.set(ShellOption::Pipefail, true);
        let argv = vec!["set".to_string(), "-o".to_string()];
        let (status, output) = run_with_captured_stdout(argv, &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(output.contains("pipefail on"), "got: {output:?}");
    }

    #[test]
    fn set_plus_o_lists_reinputable_commands() {
        let mut proxy = TestShellProxy::default();
        let argv = vec!["set".to_string(), "+o".to_string()];
        let (status, output) = run_with_captured_stdout(argv, &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(output.contains("set +o pipefail"), "got: {output:?}");

        proxy.shell_options.set(ShellOption::Pipefail, true);
        let argv = vec!["set".to_string(), "+o".to_string()];
        let (status, output) = run_with_captured_stdout(argv, &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(output.contains("set -o pipefail"), "got: {output:?}");
    }

    #[test]
    fn set_o_unknown_is_error() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        for argv in [
            vec!["set".to_string(), "-o".to_string(), "errexit".to_string()],
            vec!["set".to_string(), "+o".to_string(), "nounset".to_string()],
        ] {
            assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(1));
        }
        assert!(!proxy.shell_options.enabled(ShellOption::Pipefail));
        close_null_ctx(&ctx);
    }

    #[test]
    fn legacy_set_key_value_still_works() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        let argv = vec!["set".to_string(), "FOO".to_string(), "bar".to_string()];
        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(0));
        assert_eq!(proxy.vars.get("FOO"), Some(&"bar".to_string()));
        close_null_ctx(&ctx);
    }

    #[test]
    fn legacy_set_x_key_value_still_exports() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        let argv = vec![
            "set".to_string(),
            "-x".to_string(),
            "FOO".to_string(),
            "bar".to_string(),
        ];
        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(0));
        assert_eq!(proxy.exported.get("FOO"), Some(&"bar".to_string()));
        close_null_ctx(&ctx);
    }

    #[test]
    fn empty_argv_shows_usage_without_panicking() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        assert_eq!(
            command(&ctx, Vec::new(), &mut proxy),
            ExitStatus::ExitedWith(0)
        );
        close_null_ctx(&ctx);
    }

    #[test]
    fn bare_set_x_is_unsupported_not_silent_success() {
        let ctx = null_ctx();
        let mut proxy = TestShellProxy::default();
        let argv = vec!["set".to_string(), "-x".to_string()];
        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(1));

        let argv = vec!["set".to_string(), "-e".to_string()];
        assert_eq!(command(&ctx, argv, &mut proxy), ExitStatus::ExitedWith(1));
        close_null_ctx(&ctx);
    }
}
