use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct AbbrEntry {
    name: String,
    expansion: String,
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
    use std::collections::HashMap;

    struct MockShellProxy {
        abbreviations: HashMap<String, String>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                abbreviations: HashMap::new(),
            }
        }
    }

    impl ShellProxy for MockShellProxy {
        fn get_current_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            Ok(std::env::current_dir()?)
        }
        fn exit_shell(&mut self) {}
        fn dispatch(
            &mut self,
            _ctx: &Context,
            _cmd: &str,
            _argv: Vec<String>,
        ) -> anyhow::Result<()> {
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
        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }

        fn add_abbr(&mut self, name: String, expansion: String) {
            self.abbreviations.insert(name, expansion);
        }

        fn remove_abbr(&mut self, name: &str) -> bool {
            self.abbreviations.remove(name).is_some()
        }

        fn list_abbrs(&self) -> Vec<(String, String)> {
            self.abbreviations
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }

        fn get_abbr(&self, name: &str) -> Option<String> {
            self.abbreviations.get(name).cloned()
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
    fn test_add_abbreviation() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::new();
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
        let mut proxy = MockShellProxy::new();
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
        let mut proxy = MockShellProxy::new();
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = add_abbreviation(&ctx, "invalid name", "command", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }
}
