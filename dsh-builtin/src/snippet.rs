use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct SnippetEntry {
    name: String,
    command: String,
    description: String,
    #[tabled(rename = "uses")]
    use_count: i64,
}

/// Built-in snippet command description
pub fn description() -> &'static str {
    "Manage command snippets"
}

/// Built-in snippet command implementation
/// Manages command snippets with support for adding, listing, running, and removing snippets
///
/// Usage:
///   snippet add <name> <command>    - Add a new snippet
///   snippet remove <name>           - Remove a snippet
///   snippet list                    - List all snippets
///   snippet run <name>              - Run a snippet
///   snippet edit <name>             - Edit a snippet in external editor
///   snippet <name>                  - Show specific snippet details
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match argv.len() {
        // "snippet" - list all snippets
        1 => list_all_snippets(ctx, proxy),

        // "snippet <subcommand|name>"
        2 => {
            let arg = &argv[1];
            match arg.as_str() {
                "list" | "-l" | "--list" => list_all_snippets(ctx, proxy),
                "help" | "-h" | "--help" => show_help(ctx),
                _ => {
                    // Show specific snippet
                    show_specific_snippet(ctx, arg, proxy)
                }
            }
        }

        // "snippet <subcommand> <name>"
        3 => {
            let subcommand = &argv[1];
            let name = &argv[2];

            match subcommand.as_str() {
                "remove" | "rm" | "-r" => remove_snippet(ctx, name, proxy),
                "run" | "exec" | "-x" => run_snippet(ctx, name, proxy),
                "edit" | "-e" => edit_snippet(ctx, name, proxy),
                "add" | "-a" => {
                    ctx.write_stderr("snippet: add requires a command").ok();
                    ctx.write_stderr("Usage: snippet add <name> <command>").ok();
                    ExitStatus::ExitedWith(1)
                }
                _ => {
                    ctx.write_stderr(&format!("snippet: unknown subcommand '{subcommand}'"))
                        .ok();
                    show_help(ctx)
                }
            }
        }

        // "snippet add <name> <command>" or "snippet add <name> multi word command"
        _ => {
            if argv.len() >= 4 && (argv[1] == "add" || argv[1] == "-a") {
                let name = &argv[2];
                let command = argv[3..].join(" ");
                add_snippet(ctx, name, &command, None, proxy)
            } else {
                ctx.write_stderr("snippet: invalid arguments").ok();
                show_help(ctx)
            }
        }
    }
}

/// Show help
fn show_help(ctx: &Context) -> ExitStatus {
    let help = r#"Usage: snippet <subcommand> [arguments]

Subcommands:
  add <name> <command>    Add a new snippet
  remove <name>           Remove a snippet (aliases: rm, -r)
  list                    List all snippets (aliases: -l, --list)
  run <name>              Run a snippet (aliases: exec, -x)
  edit <name>             Edit a snippet in external editor (aliases: -e)
  <name>                  Show specific snippet details

Examples:
  snippet add deploy "kubectl apply -f deployments/"
  snippet add test "cargo test --all"
  snippet run deploy
  snippet list"#;
    ctx.write_stdout(help).ok();
    ExitStatus::ExitedWith(0)
}

/// List all snippets in a formatted table
fn list_all_snippets(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let snippets = proxy.list_snippets();

    if snippets.is_empty() {
        ctx.write_stdout("No snippets defined. Use 'snippet add <name> <command>' to create one.")
            .ok();
        return ExitStatus::ExitedWith(0);
    }

    let entries: Vec<SnippetEntry> = snippets
        .into_iter()
        .map(|s| SnippetEntry {
            name: s.name,
            command: if s.command.len() > 50 {
                format!("{}...", &s.command[..47])
            } else {
                s.command
            },
            description: s.description.unwrap_or_default(),
            use_count: s.use_count,
        })
        .collect();

    let table = Table::new(entries).to_string();
    ctx.write_stdout(&table).ok();
    ExitStatus::ExitedWith(0)
}

/// Show a specific snippet's details
fn show_specific_snippet(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(snippet) = proxy.get_snippet(name) {
        ctx.write_stdout(&format!("Name: {}", snippet.name)).ok();
        ctx.write_stdout(&format!("Command: {}", snippet.command))
            .ok();
        if let Some(desc) = snippet.description {
            ctx.write_stdout(&format!("Description: {}", desc)).ok();
        }
        ctx.write_stdout(&format!("Uses: {}", snippet.use_count))
            .ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("snippet: no snippet named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Add a new snippet
fn add_snippet(
    ctx: &Context,
    name: &str,
    command: &str,
    description: Option<&str>,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    // Validate snippet name (no spaces, special characters)
    if name.is_empty() || name.contains(' ') || name.contains('\t') {
        ctx.write_stderr("snippet: invalid snippet name (cannot contain spaces)")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    if command.is_empty() {
        ctx.write_stderr("snippet: command cannot be empty").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Check if snippet already exists
    if proxy.get_snippet(name).is_some() {
        ctx.write_stderr(&format!(
            "snippet: snippet '{name}' already exists. Use 'snippet edit {name}' to modify it."
        ))
        .ok();
        return ExitStatus::ExitedWith(1);
    }

    if proxy.add_snippet(
        name.to_string(),
        command.to_string(),
        description.map(|s| s.to_string()),
    ) {
        ctx.write_stdout(&format!("✓ Added snippet: {name}")).ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("snippet: failed to add snippet '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Remove a snippet
fn remove_snippet(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if proxy.remove_snippet(name) {
        ctx.write_stdout(&format!("✓ Removed snippet: {name}")).ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("snippet: no snippet named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Run a snippet
fn run_snippet(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(snippet) = proxy.get_snippet(name) {
        // Record usage
        proxy.record_snippet_use(name);

        // Execute the command via shell
        ctx.write_stdout(&format!("▶ Running: {}", snippet.command))
            .ok();
        match proxy.dispatch(
            ctx,
            "sh",
            vec!["sh".to_string(), "-c".to_string(), snippet.command],
        ) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                ctx.write_stderr(&format!("snippet: execution failed: {e}"))
                    .ok();
                ExitStatus::ExitedWith(1)
            }
        }
    } else {
        ctx.write_stderr(&format!("snippet: no snippet named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

/// Edit a snippet in external editor
fn edit_snippet(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(snippet) = proxy.get_snippet(name) {
        // Open the command in external editor
        match proxy.open_editor(&snippet.command, "sh") {
            Ok(new_command) => {
                let new_command = new_command.trim();
                if new_command.is_empty() {
                    ctx.write_stderr("snippet: command cannot be empty").ok();
                    return ExitStatus::ExitedWith(1);
                }
                if new_command != snippet.command {
                    if proxy.update_snippet(name, new_command, snippet.description.as_deref()) {
                        ctx.write_stdout(&format!("✓ Updated snippet: {name}")).ok();
                    } else {
                        ctx.write_stderr(&format!("snippet: failed to update snippet '{name}'"))
                            .ok();
                        return ExitStatus::ExitedWith(1);
                    }
                } else {
                    ctx.write_stdout("No changes made").ok();
                }
                ExitStatus::ExitedWith(0)
            }
            Err(e) => {
                ctx.write_stderr(&format!("snippet: failed to open editor: {e}"))
                    .ok();
                ExitStatus::ExitedWith(1)
            }
        }
    } else {
        ctx.write_stderr(&format!("snippet: no snippet named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Simple snippet struct for testing
    #[derive(Clone)]
    struct TestSnippet {
        name: String,
        command: String,
        description: Option<String>,
        use_count: i64,
    }

    struct MockShellProxy {
        snippets: HashMap<String, TestSnippet>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                snippets: HashMap::new(),
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
        fn unset_env_var(&mut self, _key: &str) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }

        fn add_abbr(&mut self, _name: String, _expansion: String) {}
        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }
        fn list_abbrs(&self) -> Vec<(String, String)> {
            vec![]
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

        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }
        fn get_git_branch(&self) -> Option<String> {
            None
        }
        fn get_job_count(&self) -> usize {
            0
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }

        // Snippet methods
        fn add_snippet(
            &mut self,
            name: String,
            command: String,
            description: Option<String>,
        ) -> bool {
            self.snippets.insert(
                name.clone(),
                TestSnippet {
                    name,
                    command,
                    description,
                    use_count: 0,
                },
            );
            true
        }
        fn remove_snippet(&mut self, name: &str) -> bool {
            self.snippets.remove(name).is_some()
        }
        fn list_snippets(&self) -> Vec<dsh_types::snippet::Snippet> {
            self.snippets
                .values()
                .map(|s| dsh_types::snippet::Snippet {
                    id: 0,
                    name: s.name.clone(),
                    command: s.command.clone(),
                    description: s.description.clone(),
                    tags: None,
                    created_at: 0,
                    last_used: None,
                    use_count: s.use_count,
                })
                .collect()
        }
        fn get_snippet(&self, name: &str) -> Option<dsh_types::snippet::Snippet> {
            self.snippets
                .get(name)
                .map(|s| dsh_types::snippet::Snippet {
                    id: 0,
                    name: s.name.clone(),
                    command: s.command.clone(),
                    description: s.description.clone(),
                    tags: None,
                    created_at: 0,
                    last_used: None,
                    use_count: s.use_count,
                })
        }
        fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool {
            if let Some(s) = self.snippets.get_mut(name) {
                s.command = command.to_string();
                s.description = description.map(|d| d.to_string());
                true
            } else {
                false
            }
        }
        fn record_snippet_use(&mut self, name: &str) {
            if let Some(s) = self.snippets.get_mut(name) {
                s.use_count += 1;
            }
        }
    }

    #[test]
    fn test_add_snippet() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::new();
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = add_snippet(&ctx, "test", "echo hello", None, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert!(proxy.get_snippet("test").is_some());
    }

    #[test]
    fn test_remove_snippet() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::new();
        proxy.add_snippet("test".to_string(), "echo hello".to_string(), None);

        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = remove_snippet(&ctx, "test", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert!(proxy.get_snippet("test").is_none());
    }

    #[test]
    fn test_invalid_snippet_name() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::new();
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = add_snippet(&ctx, "invalid name", "command", None, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }
}
