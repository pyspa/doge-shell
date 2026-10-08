use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::path::Path;

/// Built-in cd command description
pub fn description() -> &'static str {
    "Change the current working directory"
}

/// Built-in cd (change directory) command implementation
/// Supports various path formats:
/// - Absolute paths (starting with /)
/// - Home directory paths (starting with ~)
/// - Relative paths
/// - `-` for the previous directory (`$OLDPWD`)
/// - `-N` / `+N` for entry N of the directory stack (as numbered by `dirs -v`)
/// - No argument (uses logical `HOME`; missing/empty `HOME` fails without moving)
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // `cd -N` / `cd +N` jump into the directory stack. Checked before the
    // `-` arm below, which is plain $OLDPWD and must keep working.
    if let Some(index) = argv
        .get(1)
        .and_then(|arg| crate::dirstack::parse_stack_index(arg))
    {
        return match crate::dirstack::goto_stack_index(proxy, index) {
            Ok(path) => {
                ctx.write_stdout(&path).ok();
                ExitStatus::ExitedWith(0)
            }
            Err(err) => {
                ctx.write_stderr(&format!("cd: {err}")).ok();
                ExitStatus::ExitedWith(1)
            }
        };
    }

    // Get current directory for relative path resolution
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            ctx.write_stderr(&format!("cd: failed to get current directory: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // Determine target directory based on argument
    let dir = match argv.get(1).map(|s| s.as_str()) {
        // Handle absolute paths (starting with /)
        Some(dir) if dir.starts_with('/') => dir.to_string(),

        // Handle home directory paths (starting with ~)
        Some(dir) if dir.starts_with('~') => shellexpand::tilde(dir).to_string(),

        // Handle previous directory (cd -)
        Some("-") => match proxy.get_var("OLDPWD") {
            Some(old_pwd) => old_pwd,
            None => {
                ctx.write_stderr("cd: OLDPWD not set").ok();
                return ExitStatus::ExitedWith(1);
            }
        },

        // Handle relative paths
        Some(dir) => {
            let res = Path::new(&current_dir).join(dir).canonicalize();

            match res {
                Ok(res) => res.to_string_lossy().into_owned(),
                Err(err) => {
                    ctx.write_stderr(&format!("cd: {err}: {dir}")).ok();
                    return ExitStatus::ExitedWith(1);
                }
            }
        }

        // No argument provided - default to home directory
        None => match proxy.get_var("HOME") {
            Some(home) if !home.is_empty() => home,
            _ => {
                ctx.write_stderr("cd: HOME not set or empty").ok();
                return ExitStatus::ExitedWith(1);
            }
        },
    };

    // Attempt to change directory through shell proxy
    match proxy.changepwd(&dir) {
        Ok(_) => {
            if argv.get(1).is_some_and(|arg| arg == "-") {
                ctx.write_stdout(&dir).ok();
            }
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            ctx.write_stderr(&format!("cd: {err}: {dir}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::AsRawFd;

    #[test]
    fn no_argument_passes_logical_home_without_reinterpreting_it() {
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);
        for home in ["/logical home", "relative", "~", "-"] {
            let mut proxy = TestShellProxy {
                allow_changepwd: true,
                ..Default::default()
            };
            proxy.vars.insert("HOME".into(), home.into());
            assert_eq!(
                command(&ctx, vec!["cd".into()], &mut proxy),
                ExitStatus::ExitedWith(0)
            );
            assert_eq!(proxy.changed_to.as_deref(), Some(home));
        }
    }

    #[test]
    fn missing_or_empty_logical_home_does_not_call_changepwd() {
        let pid = nix::unistd::getpid();
        for home in [None, Some("")] {
            let mut errors = tempfile::tempfile().unwrap();
            let mut ctx = Context::new_safe(pid, pid, false);
            ctx.errfile = errors.as_raw_fd();
            let mut proxy = TestShellProxy {
                allow_changepwd: true,
                ..Default::default()
            };
            if let Some(home) = home {
                proxy.vars.insert("HOME".into(), home.into());
            }
            assert_eq!(
                command(&ctx, vec!["cd".into()], &mut proxy),
                ExitStatus::ExitedWith(1)
            );
            assert_eq!(proxy.changed_to, None);
            errors.seek(SeekFrom::Start(0)).unwrap();
            let mut diagnostic = String::new();
            errors.read_to_string(&mut diagnostic).unwrap();
            assert!(diagnostic.contains("cd: HOME not set or empty"));
        }
    }

    #[test]
    fn explicit_directory_does_not_require_home() {
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);
        let mut proxy = TestShellProxy {
            allow_changepwd: true,
            ..Default::default()
        };
        assert_eq!(
            command(&ctx, vec!["cd".into(), "/explicit".into()], &mut proxy),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(proxy.changed_to.as_deref(), Some("/explicit"));
    }

    #[test]
    fn cd_minus_prints_captured_destination_only_after_success() {
        let pid = nix::unistd::getpid();
        for succeeds in [false, true] {
            let mut stdout = tempfile::tempfile().unwrap();
            let stderr = tempfile::tempfile().unwrap();
            let mut ctx = Context::new_safe(pid, pid, false);
            ctx.outfile = stdout.as_raw_fd();
            ctx.errfile = stderr.as_raw_fd();
            let mut proxy = TestShellProxy {
                allow_changepwd: succeeds,
                ..Default::default()
            };
            proxy
                .vars
                .insert("OLDPWD".into(), "/previous target".into());
            assert_eq!(
                command(&ctx, vec!["cd".into(), "-".into()], &mut proxy),
                ExitStatus::ExitedWith(if succeeds { 0 } else { 1 })
            );
            stdout.seek(SeekFrom::Start(0)).unwrap();
            let mut printed = String::new();
            stdout.read_to_string(&mut printed).unwrap();
            assert_eq!(printed, if succeeds { "/previous target\n" } else { "" });
        }
    }
}
