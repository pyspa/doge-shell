use super::ShellProxy;
use crate::text::truncate_preview;
use dsh_types::{Context, ExitStatus};
use std::borrow::Cow;
use tabled::{Table, Tabled};

struct SnippetEntry {
    name: String,
    command: String,
    description: String,
    use_count: i64,
}

impl Tabled for SnippetEntry {
    const LENGTH: usize = 4;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(self.name.as_str()),
            Cow::Borrowed(self.command.as_str()),
            Cow::Borrowed(self.description.as_str()),
            Cow::Owned(self.use_count.to_string()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![
            Cow::Borrowed("name"),
            Cow::Borrowed("command"),
            Cow::Borrowed("description"),
            Cow::Borrowed("uses"),
        ]
    }
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
                    let _ = show_help(ctx);
                    ExitStatus::ExitedWith(1)
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
                let _ = show_help(ctx);
                ExitStatus::ExitedWith(1)
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
            command: truncate_preview(&s.command, 47),
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
        match crate::dispatch_shell_command(ctx, proxy, snippet.command) {
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
    use crate::test_support::TestShellProxy;

    #[test]
    fn test_add_snippet() {
        use nix::unistd::getpid;
        let mut proxy = TestShellProxy::default();
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
        let mut proxy = TestShellProxy::default();
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
        let mut proxy = TestShellProxy::default();
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = add_snippet(&ctx, "invalid name", "command", None, &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(1));
    }

    #[test]
    fn test_run_snippet_dispatches_shell_command_without_duplicate_sh() {
        use nix::unistd::getpid;
        let mut proxy = TestShellProxy {
            allow_dispatch: true,
            ..TestShellProxy::default()
        };
        proxy.add_snippet("test".to_string(), "echo hello".to_string(), None);
        let pid = getpid();
        let pgid = pid;
        let ctx = Context::new_safe(pid, pgid, false);

        let result = run_snippet(&ctx, "test", &mut proxy);

        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert_eq!(
            proxy.dispatched,
            vec![(
                "sh".to_string(),
                vec!["-c".to_string(), "echo hello".to_string()]
            )]
        );
    }
}
