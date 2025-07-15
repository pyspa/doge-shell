use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct AliasEntry {
    alias: String,
    command: String,
}

/// Built-in alias command implementation
/// Manages shell aliases with support for setting, listing, and querying aliases
/// Integrates with the existing Lisp-based alias system
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match argv.len() {
        // "alias" - list all aliases
        1 => list_all_aliases(ctx, proxy),

        // "alias name" or "alias name=command"
        2 => {
            let arg = &argv[1];
            if arg.contains('=') {
                // "alias name=command" - set alias
                set_alias_from_assignment(ctx, arg, proxy)
            } else {
                // "alias name" - show specific alias
                show_specific_alias(ctx, arg, proxy)
            }
        }

        // Invalid number of arguments
        _ => {
            ctx.write_stderr("alias: invalid number of arguments").ok();
            ctx.write_stderr("Usage: alias [name[=command]]").ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Lists all current aliases in a formatted table
fn list_all_aliases(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let aliases = proxy.list_aliases();

    if aliases.is_empty() {
        ctx.write_stdout("No aliases defined").ok();
        return ExitStatus::ExitedWith(0);
    }

    // Convert HashMap to sorted vector for consistent output
    let mut alias_entries: Vec<AliasEntry> = aliases
        .into_iter()
        .map(|(alias, command)| AliasEntry { alias, command })
        .collect();

    // Sort by alias name for consistent output
    alias_entries.sort_by(|a, b| a.alias.cmp(&b.alias));

    // Create and display formatted table
    let table = Table::new(alias_entries).to_string();
    match ctx.write_stdout(&table) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            ctx.write_stderr(&format!("alias: failed to display aliases: {err}"))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Shows a specific alias value
fn show_specific_alias(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.get_alias(name) {
        Some(command) => {
            // Display in format: name='command'
            let output = format!("{name}='{command}'");
            match ctx.write_stdout(&output) {
                Ok(_) => ExitStatus::ExitedWith(0),
                Err(err) => {
                    ctx.write_stderr(&format!("alias: failed to display alias: {err}"))
                        .ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }
        None => {
            ctx.write_stderr(&format!("alias: {name}: not found")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Sets an alias from a "name=command" assignment string
fn set_alias_from_assignment(
    ctx: &Context,
    assignment: &str,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    // Parse the assignment string
    let parts: Vec<&str> = assignment.splitn(2, '=').collect();
    if parts.len() != 2 {
        ctx.write_stderr("alias: invalid assignment format").ok();
        return ExitStatus::ExitedWith(1);
    }

    let name = parts[0].trim();
    let command = parts[1].trim();

    // Validate alias name
    if name.is_empty() {
        ctx.write_stderr("alias: empty alias name").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Validate command (allow empty for unsetting, but warn)
    if command.is_empty() {
        ctx.write_stderr("alias: empty command (use unalias to remove aliases)")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    // Remove surrounding quotes if present
    let command = remove_surrounding_quotes(command);

    // Validate alias name format (basic validation)
    if !is_valid_alias_name(name) {
        ctx.write_stderr(&format!("alias: invalid alias name: {name}"))
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    // Set the alias
    proxy.set_alias(name.to_string(), command.to_string());
    ExitStatus::ExitedWith(0)
}

/// Removes surrounding single or double quotes from a string
fn remove_surrounding_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        return &s[1..s.len() - 1];
    }
    s
}

/// Validates alias name format
/// Alias names should be valid shell identifiers
fn is_valid_alias_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    // First character must be alphabetic or underscore
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if !first.is_alphabetic() && first != '_' {
            return false;
        }
    }

    // Remaining characters must be alphanumeric, underscore, or hyphen
    for c in chars {
        if !c.is_alphanumeric() && c != '_' && c != '-' {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Mock ShellProxy for testing
    struct MockShellProxy {
        aliases: HashMap<String, String>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                aliases: HashMap::new(),
            }
        }
    }

    impl ShellProxy for MockShellProxy {
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

        fn get_alias(&mut self, name: &str) -> Option<String> {
            self.aliases.get(name).cloned()
        }

        fn set_alias(&mut self, name: String, command: String) {
            self.aliases.insert(name, command);
        }

        fn list_aliases(&mut self) -> HashMap<String, String> {
            self.aliases.clone()
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
    }

    #[test]
    fn test_remove_surrounding_quotes() {
        assert_eq!(remove_surrounding_quotes("\"hello world\""), "hello world");
        assert_eq!(remove_surrounding_quotes("'hello world'"), "hello world");
        assert_eq!(remove_surrounding_quotes("hello world"), "hello world");
        assert_eq!(remove_surrounding_quotes("\"hello"), "\"hello");
        assert_eq!(remove_surrounding_quotes("hello\""), "hello\"");
        assert_eq!(remove_surrounding_quotes("\"\""), "");
        assert_eq!(remove_surrounding_quotes("''"), "");
    }

    #[test]
    fn test_is_valid_alias_name() {
        assert!(is_valid_alias_name("ll"));
        assert!(is_valid_alias_name("ls_long"));
        assert!(is_valid_alias_name("git-status"));
        assert!(is_valid_alias_name("_private"));
        assert!(is_valid_alias_name("cmd123"));

        assert!(!is_valid_alias_name(""));
        assert!(!is_valid_alias_name("123cmd"));
        assert!(!is_valid_alias_name("cmd with spaces"));
        assert!(!is_valid_alias_name("cmd@special"));
        assert!(!is_valid_alias_name("cmd=value"));
    }

    #[test]
    fn test_alias_command_set_and_get() {
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            unsafe { std::mem::zeroed() },
            false,
        );
        let mut proxy = MockShellProxy::new();

        // Test setting an alias
        let argv = vec!["alias".to_string(), "ll=ls -la".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));

        // Verify alias was set
        assert_eq!(proxy.get_alias("ll"), Some("ls -la".to_string()));

        // Test getting specific alias
        let argv = vec!["alias".to_string(), "ll".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
    }

    #[test]
    fn test_alias_command_list_empty() {
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            unsafe { std::mem::zeroed() },
            false,
        );
        let mut proxy = MockShellProxy::new();

        // Test listing empty aliases
        let argv = vec!["alias".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
    }

    #[test]
    fn test_alias_command_invalid_name() {
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            unsafe { std::mem::zeroed() },
            false,
        );
        let mut proxy = MockShellProxy::new();

        // Test invalid alias name
        let argv = vec!["alias".to_string(), "123invalid=command".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_alias_command_not_found() {
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            unsafe { std::mem::zeroed() },
            false,
        );
        let mut proxy = MockShellProxy::new();

        // Test getting non-existent alias
        let argv = vec!["alias".to_string(), "nonexistent".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_alias_command_empty_command() {
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            unsafe { std::mem::zeroed() },
            false,
        );
        let mut proxy = MockShellProxy::new();

        // Test empty command
        let argv = vec!["alias".to_string(), "test=".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_alias_command_with_quotes() {
        use nix::unistd::Pid;
        let ctx = Context::new(
            Pid::from_raw(0),
            Pid::from_raw(0),
            unsafe { std::mem::zeroed() },
            false,
        );
        let mut proxy = MockShellProxy::new();

        // Test alias with quoted command
        let argv = vec!["alias".to_string(), "ll='ls -la'".to_string()];
        let result = command(&ctx, argv, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));

        // Verify quotes were removed
        assert_eq!(proxy.get_alias("ll"), Some("ls -la".to_string()));
    }
}
