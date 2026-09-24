use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in add_path command description
pub fn description() -> &'static str {
    "Add paths to the PATH environment variable"
}

/// Built-in add_path command implementation
/// Adds a directory to the beginning of the PATH environment variable
/// Supports tilde expansion for home directory references
pub fn command(ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let Some(arg) = args.get(1) else {
        ctx.write_stderr("usage: add_path <directory>").ok();
        return ExitStatus::ExitedWith(1);
    };
    // PATH mutation and tilde expansion live in the Environment's canonical
    // insertion path; the builtin only delegates so the logical PATH
    // variable, command cache, and completion activation stay in sync.
    proxy.insert_path(0, arg);
    ExitStatus::ExitedWith(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    fn test_context() -> Context {
        let pid = nix::unistd::getpid();
        Context::new_safe(pid, pid, false)
    }

    fn run(argv: &[&str], proxy: &mut TestShellProxy) -> ExitStatus {
        let owned: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        command(&test_context(), owned, proxy)
    }

    #[test]
    fn reports_usage_without_an_argument() {
        let mut proxy = TestShellProxy::default();
        let status = run(&["add_path"], &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert_eq!(proxy.insert_path_calls, 0);
        assert!(proxy.inserted_paths.is_empty());
    }

    #[test]
    fn delegates_the_raw_argument_to_the_proxy() {
        let mut proxy = TestShellProxy::default();
        let status = run(&["add_path", "/tmp/bin"], &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.insert_path_calls, 1);
        assert_eq!(proxy.inserted_paths, vec![(0, "/tmp/bin".to_string())]);
    }

    #[test]
    fn passes_tilde_through_for_the_environment_to_expand() {
        let mut proxy = TestShellProxy::default();
        let status = run(&["add_path", "~/bin"], &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        // No builtin-side expansion: the Environment resolves `~` against
        // the logical shell HOME.
        assert_eq!(proxy.inserted_paths, vec![(0, "~/bin".to_string())]);
    }
}
