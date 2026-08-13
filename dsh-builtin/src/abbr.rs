use super::{CoreShellAction, ShellProxy};
use crate::capability::ExecutionCapability;
use dsh_types::{Context, ExitStatus};
use std::borrow::Cow;
use tabled::{Table, Tabled};

struct AbbrEntry {
    name: String,
    expansion: String,
}

impl Tabled for AbbrEntry {
    const LENGTH: usize = 2;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(self.name.as_str()),
            Cow::Borrowed(self.expansion.as_str()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed("name"), Cow::Borrowed("expansion")]
    }
}

/// Built-in abbr command description
pub fn description() -> &'static str {
    "Manage abbreviations that expand when typed"
}

/// Built-in abbr command implementation
/// Manages shell abbreviations with support for setting, listing, and removing abbreviations
/// Integrates with the Lisp-based abbreviation system and provides real-time expansion
///
/// Usage:
///   abbr -a name expansion    - Add abbreviation
///   abbr -e name             - Erase abbreviation  
///   abbr -l                  - List all abbreviations
///   abbr -s                  - Show all abbreviations (same as -l)
///   abbr name                - Show specific abbreviation
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(command_index) = argv.iter().position(|arg| arg == "--command") {
        return add_command_abbreviation(ctx, &argv, command_index, proxy);
    }
    match argv.len() {
        // "abbr" - list all abbreviations
        1 => list_all_abbreviations(ctx, proxy),

        // "abbr -l", "abbr -s", "abbr -e name", "abbr name"
        2 => {
            let arg = &argv[1];
            match arg.as_str() {
                "-l" | "-s" | "--list" => list_all_abbreviations(ctx, proxy),
                _ => {
                    if arg.starts_with("-e") {
                        ctx.write_stderr("abbr: -e option requires abbreviation name")
                            .ok();
                        ctx.write_stderr("Usage: abbr -e <name>").ok();
                        ExitStatus::ExitedWith(1)
                    } else {
                        // Show specific abbreviation
                        show_specific_abbreviation(ctx, arg, proxy)
                    }
                }
            }
        }

        // "abbr -e name", "abbr -a name expansion"
        3 => {
            let option = &argv[1];
            let name = &argv[2];

            match option.as_str() {
                "-e" | "--erase" => remove_abbreviation(ctx, name, proxy),
                "-a" | "--add" => {
                    ctx.write_stderr("abbr: -a option requires expansion").ok();
                    ctx.write_stderr("Usage: abbr -a <name> <expansion>").ok();
                    ExitStatus::ExitedWith(1)
                }
                _ => {
                    ctx.write_stderr("abbr: invalid option").ok();
                    ctx.write_stderr("Usage: abbr [-a|-e|-l|-s] [name] [expansion]")
                        .ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }

        // "abbr -a name expansion" (minimum case)
        4 => {
            let option = &argv[1];
            let name = &argv[2];
            let expansion = &argv[3];

            match option.as_str() {
                "-a" | "--add" => add_abbreviation(ctx, name, expansion, proxy),
                _ => {
                    ctx.write_stderr("abbr: invalid option").ok();
                    ctx.write_stderr("Usage: abbr [-a|-e|-l|-s] [name] [expansion]")
                        .ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }

        // "abbr -a name multi word expansion"
        _ => {
            if argv.len() > 4 && (argv[1] == "-a" || argv[1] == "--add") {
                let name = &argv[2];
                let expansion = argv[3..].join(" ");
                add_abbreviation(ctx, name, &expansion, proxy)
            } else {
                ctx.write_stderr("abbr: too many arguments").ok();
                ctx.write_stderr("Usage: abbr [-a|-e|-l|-s] [name] [expansion]")
                    .ok();
                ExitStatus::ExitedWith(1)
            }
        }
    }
}

fn add_command_abbreviation(
    ctx: &Context,
    argv: &[String],
    command_index: usize,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    let is_add = argv.iter().any(|arg| arg == "-a" || arg == "--add");
    let Some(command_scope) = argv.get(command_index + 1) else {
        let _ = ctx.write_stderr("abbr: --command requires a command name");
        return ExitStatus::ExitedWith(1);
    };
    let remaining = &argv[command_index + 2..];
    if !is_add || remaining.len() < 2 {
        let _ = ctx.write_stderr("Usage: abbr --add --command <command> <name> <expansion>");
        return ExitStatus::ExitedWith(1);
    }
    let name = &remaining[0];
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        let _ = ctx.write_stderr("abbr: invalid abbreviation name (cannot contain spaces)");
        return ExitStatus::ExitedWith(1);
    }
    let expansion = remaining[1..].join(" ");
    let action_argv = vec![
        "abbr-command".to_string(),
        command_scope.clone(),
        name.clone(),
        expansion.clone(),
    ];
    match proxy.dispatch_core(ctx, CoreShellAction::AbbrCommand, action_argv) {
        Ok(()) => {
            let _ = ctx.write_stdout(&format!(
                "Added abbreviation for {command_scope}: {name} → {expansion}"
            ));
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!("abbr: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}

/// List all abbreviations in a formatted table
fn list_all_abbreviations(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let abbreviations = proxy.list_abbrs();

    if abbreviations.is_empty() {
        ctx.write_stdout("No abbreviations defined").ok();
        return ExitStatus::ExitedWith(0);
    }

    let entries: Vec<AbbrEntry> = abbreviations
        .into_iter()
        .map(|(name, expansion)| AbbrEntry { name, expansion })
        .collect();

    let table = Table::new(entries).to_string();
    ctx.write_stdout(&table).ok();
    ExitStatus::ExitedWith(0)
}

/// Show a specific abbreviation
fn show_specific_abbreviation(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(expansion) = proxy.get_abbr(name) {
        ctx.write_stdout(&format!("abbr {name} '{expansion}'")).ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("abbr: no abbreviation named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Add a new abbreviation
fn add_abbreviation(
    ctx: &Context,
    name: &str,
    expansion: &str,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    // Validate abbreviation name (no spaces, special characters)
    if name.is_empty() || name.contains(' ') || name.contains('\t') {
        ctx.write_stderr("abbr: invalid abbreviation name (cannot contain spaces)")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    proxy.add_abbr(name.to_string(), expansion.to_string());
    ctx.write_stdout(&format!("Added abbreviation: {name} → {expansion}"))
        .ok();
    ExitStatus::ExitedWith(0)
}

/// Remove an abbreviation
fn remove_abbreviation(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if proxy.remove_abbr(name) {
        ctx.write_stdout(&format!("Removed abbreviation: {name}"))
            .ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("abbr: no abbreviation named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    type MockShellProxy = TestShellProxy;

    #[test]
    fn test_add_abbreviation() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::default();
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = add_abbreviation(&ctx, "gco", "git checkout", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.get_abbr("gco"), Some("git checkout".to_string()));
    }

    #[test]
    fn test_remove_abbreviation() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::default();
        proxy.add_abbr("gco".to_string(), "git checkout".to_string());
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = remove_abbreviation(&ctx, "gco", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.get_abbr("gco"), None);
    }

    #[test]
    fn test_invalid_abbreviation_name() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::default();
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = add_abbreviation(&ctx, "invalid name", "command", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn command_scoped_form_uses_typed_core_action() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy {
            allow_dispatch: true,
            ..Default::default()
        };
        let pid = getpid();
        let ctx = Context::new_safe(pid, pid, false);
        let status = command(
            &ctx,
            vec![
                "abbr".to_string(),
                "--add".to_string(),
                "--command".to_string(),
                "git".to_string(),
                "co".to_string(),
                "checkout".to_string(),
            ],
            &mut proxy,
        );
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(
            proxy.dispatched,
            vec![(
                "abbr-command".to_string(),
                vec![
                    "abbr-command".to_string(),
                    "git".to_string(),
                    "co".to_string(),
                    "checkout".to_string()
                ]
            )]
        );
    }
}
