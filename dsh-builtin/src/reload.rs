use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in reload command description
pub fn description() -> &'static str {
    "Reload shell configuration"
}

/// Built-in reload command implementation
/// Reloads the config.lisp configuration file without restarting the shell
/// This allows users to apply configuration changes during their shell session
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Parse command-line arguments
    match argv.len() {
        // "reload" - perform reload
        1 => perform_reload(ctx, proxy),

        // "reload --help" or "reload -h" - show help
        2 => {
            let arg = &argv[1];
            if arg == "--help" || arg == "-h" {
                show_help(ctx)
            } else {
                show_invalid_argument_error(ctx, arg)
            }
        }

        // Invalid number of arguments
        _ => {
            ctx.write_stderr("reload: too many arguments").ok();
            show_usage(ctx);
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Performs the actual config reload by delegating to the shell
fn perform_reload(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "reload", vec!["reload".to_string()]) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            ctx.write_stderr(&format!("reload: {err}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Shows help information for the reload command
fn show_help(ctx: &Context) -> ExitStatus {
    let help_text = r#"reload - reload configuration file

USAGE:
    reload [--help|-h]

DESCRIPTION:
    Reloads the config.lisp configuration file without restarting the shell.
    This allows you to apply configuration changes during your shell session.

    The command re-reads and re-executes the config.lisp file from the
    standard configuration directory (~/.config/dsh/config.lisp).

OPTIONS:
    -h, --help    Show this help message

EXAMPLES:
    reload        Reload the configuration file
    reload -h     Show this help message

EXIT STATUS:
    0    Configuration reloaded successfully
    1    Error occurred during reload (file not found, syntax error, etc.)

NOTES:
    - If the config file contains syntax errors, they will be displayed
    - If the config file cannot be read, an appropriate error message is shown
    - The current shell state is preserved if reload fails
    - New aliases and settings are applied immediately on successful reload"#;

    match ctx.write_stdout(help_text) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(_) => ExitStatus::ExitedWith(1),
    }
}

/// Shows error message for invalid arguments
fn show_invalid_argument_error(ctx: &Context, arg: &str) -> ExitStatus {
    ctx.write_stderr(&format!("reload: invalid option: {arg}"))
        .ok();
    show_usage(ctx);
    ExitStatus::ExitedWith(1)
}

/// Shows usage information
fn show_usage(ctx: &Context) {
    ctx.write_stderr("Usage: reload [--help|-h]").ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Mock ShellProxy for testing
    struct MockShellProxy {
        dispatch_result: Result<(), anyhow::Error>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                dispatch_result: Ok(()),
            }
        }

        fn with_error(error: anyhow::Error) -> Self {
            Self {
                dispatch_result: Err(error),
            }
        }
    }

    impl ShellProxy for MockShellProxy {
        fn exit_shell(&mut self) {}

        fn dispatch(&mut self, _ctx: &Context, cmd: &str, argv: Vec<String>) -> anyhow::Result<()> {
            assert_eq!(cmd, "reload");
            assert_eq!(argv, vec!["reload".to_string()]);
            self.dispatch_result
                .as_ref()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            Ok(())
        }

        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn insert_path(&mut self, _index: usize, _path: &str) {}
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn add_abbr(&mut self, _name: String, _expansion: String) {}
        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }
        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }

        fn list_mcp_servers(&mut self) -> Vec<dsh_types::mcp::McpServerConfig> {
            Vec::new()
        }

        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            vec![]
        }
        fn export_var(&mut self, _key: &str) -> bool {
            true
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
    }

    #[test]
    fn test_reload_command_basic() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::new();

        let argv = vec!["reload".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
    }

    #[test]
    fn test_reload_command_help_long() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::new();

        let argv = vec!["reload".to_string(), "--help".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
    }

    #[test]
    fn test_reload_command_help_short() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::new();

        let argv = vec!["reload".to_string(), "-h".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
    }

    #[test]
    fn test_reload_command_invalid_argument() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::new();

        let argv = vec!["reload".to_string(), "--invalid".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_reload_command_too_many_arguments() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::new();

        let argv = vec!["reload".to_string(), "arg1".to_string(), "arg2".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_reload_command_dispatch_error() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::with_error(anyhow::anyhow!("test error"));

        let argv = vec!["reload".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_reload_command_file_not_found_error() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::with_error(anyhow::anyhow!(
            "Failed to read config file: ~/.config/dsh/config.lisp: No such file or directory"
        ));

        let argv = vec!["reload".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_reload_command_permission_denied_error() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::with_error(anyhow::anyhow!("Permission denied"));

        let argv = vec!["reload".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_reload_command_lisp_syntax_error() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::with_error(anyhow::anyhow!(
            "Parse error: unexpected token ')' at index 15"
        ));

        let argv = vec!["reload".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_reload_command_lisp_runtime_error() {
        use nix::fcntl::{OFlag, open};
        use nix::sys::stat::Mode;
        use nix::sys::termios::{Termios, tcgetattr};
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            tcgetattr(
                open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .unwrap_or_else(|_| panic!("Cannot open /dev/tty")),
            )
            .unwrap_or_else(|e| panic!("Cannot initialize Termios for test: {}", e)),
            false,
        );
        let mut proxy = MockShellProxy::with_error(anyhow::anyhow!(
            "Runtime error: undefined function 'invalid-func'"
        ));

        let argv = vec!["reload".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }
}
