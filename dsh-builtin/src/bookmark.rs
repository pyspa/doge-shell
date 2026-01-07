use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct BookmarkEntry {
    name: String,
    command: String,
    #[tabled(rename = "uses")]
    use_count: i64,
}

/// Built-in bookmark command description
pub fn description() -> &'static str {
    "Manage command bookmarks"
}

/// Built-in bookmark command implementation
/// Manages command bookmarks for quick access to frequently used commands
///
/// Usage:
///   bookmark add <name> [command]  - Add bookmark (uses last command if omitted)
///   bookmark remove <name>         - Remove a bookmark
///   bookmark list                  - List all bookmarks
///   bookmark run <name>            - Run a bookmark
///   bookmark <name>                - Show bookmark details
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match argv.len() {
        1 => list_all_bookmarks(ctx, proxy),

        2 => {
            let arg = &argv[1];
            match arg.as_str() {
                "list" | "-l" | "--list" => list_all_bookmarks(ctx, proxy),
                "help" | "-h" | "--help" => show_help(ctx),
                _ => show_specific_bookmark(ctx, arg, proxy),
            }
        }

        3 => {
            let subcommand = &argv[1];
            let name = &argv[2];

            match subcommand.as_str() {
                "remove" | "rm" | "-r" => remove_bookmark(ctx, name, proxy),
                "run" | "exec" | "-x" => run_bookmark(ctx, name, proxy),
                "add" | "-a" => {
                    // bookmark add <name> - use last command
                    if let Some(last_cmd) = proxy.get_last_command() {
                        add_bookmark(ctx, name, &last_cmd, proxy)
                    } else {
                        ctx.write_stderr("bookmark: no command in history").ok();
                        ExitStatus::ExitedWith(1)
                    }
                }
                _ => {
                    ctx.write_stderr(&format!("bookmark: unknown subcommand '{subcommand}'"))
                        .ok();
                    show_help(ctx)
                }
            }
        }

        _ => {
            if argv.len() >= 4 && (argv[1] == "add" || argv[1] == "-a") {
                let name = &argv[2];
                let command = argv[3..].join(" ");
                add_bookmark(ctx, name, &command, proxy)
            } else {
                ctx.write_stderr("bookmark: invalid arguments").ok();
                show_help(ctx)
            }
        }
    }
}

fn show_help(ctx: &Context) -> ExitStatus {
    let help = r#"Usage: bookmark <subcommand> [arguments]

Subcommands:
  add <name> [command]    Add a bookmark (uses last command if omitted)
  remove <name>           Remove a bookmark (aliases: rm, -r)
  list                    List all bookmarks (aliases: -l, --list)
  run <name>              Run a bookmark (aliases: exec, -x)
  <name>                  Show bookmark details

Examples:
  bookmark add deploy                       # Bookmark last command as "deploy"
  bookmark add test "cargo test --all"      # Bookmark with explicit command
  bookmark run deploy
  bookmark list"#;
    ctx.write_stdout(help).ok();
    ExitStatus::ExitedWith(0)
}

fn list_all_bookmarks(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let bookmarks = proxy.list_bookmarks();

    if bookmarks.is_empty() {
        ctx.write_stdout(
            "No bookmarks defined. Use 'bookmark add <name> [command]' to create one.",
        )
        .ok();
        return ExitStatus::ExitedWith(0);
    }

    let entries: Vec<BookmarkEntry> = bookmarks
        .into_iter()
        .map(|b| BookmarkEntry {
            name: b.0,
            command: if b.1.len() > 50 {
                format!("{}...", &b.1[..47])
            } else {
                b.1
            },
            use_count: b.2,
        })
        .collect();

    let table = Table::new(entries).to_string();
    ctx.write_stdout(&table).ok();
    ExitStatus::ExitedWith(0)
}

fn show_specific_bookmark(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some((cmd, use_count)) = proxy.get_bookmark(name) {
        ctx.write_stdout(&format!("Name: {}", name)).ok();
        ctx.write_stdout(&format!("Command: {}", cmd)).ok();
        ctx.write_stdout(&format!("Uses: {}", use_count)).ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("bookmark: no bookmark named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

fn add_bookmark(
    ctx: &Context,
    name: &str,
    command: &str,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    if name.is_empty() || name.contains(' ') || name.contains('\t') {
        ctx.write_stderr("bookmark: invalid name (cannot contain spaces)")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    if command.is_empty() {
        ctx.write_stderr("bookmark: command cannot be empty").ok();
        return ExitStatus::ExitedWith(1);
    }

    if proxy.get_bookmark(name).is_some() {
        ctx.write_stderr(&format!(
            "bookmark: '{name}' already exists. Use 'bookmark remove {name}' first."
        ))
        .ok();
        return ExitStatus::ExitedWith(1);
    }

    if proxy.add_bookmark(name.to_string(), command.to_string()) {
        ctx.write_stdout(&format!("✓ Added bookmark: {} = {}", name, command))
            .ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("bookmark: failed to add '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

fn remove_bookmark(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if proxy.remove_bookmark(name) {
        ctx.write_stdout(&format!("✓ Removed bookmark: {name}"))
            .ok();
        ExitStatus::ExitedWith(0)
    } else {
        ctx.write_stderr(&format!("bookmark: no bookmark named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

fn run_bookmark(ctx: &Context, name: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some((command, _)) = proxy.get_bookmark(name) {
        proxy.record_bookmark_use(name);
        ctx.write_stdout(&format!("▶ Running: {}", command)).ok();
        match proxy.dispatch(ctx, "sh", vec!["sh".to_string(), "-c".to_string(), command]) {
            Ok(_) => ExitStatus::ExitedWith(0),
            Err(e) => {
                ctx.write_stderr(&format!("bookmark: execution failed: {e}"))
                    .ok();
                ExitStatus::ExitedWith(1)
            }
        }
    } else {
        ctx.write_stderr(&format!("bookmark: no bookmark named '{name}'"))
            .ok();
        ExitStatus::ExitedWith(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockShellProxy {
        bookmarks: std::collections::HashMap<String, (String, i64)>,
        last_command: Option<String>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                bookmarks: std::collections::HashMap::new(),
                last_command: Some("echo test".to_string()),
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

        // Bookmark methods
        fn add_bookmark(&mut self, name: String, command: String) -> bool {
            self.bookmarks.insert(name, (command, 0));
            true
        }
        fn remove_bookmark(&mut self, name: &str) -> bool {
            self.bookmarks.remove(name).is_some()
        }
        fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
            self.bookmarks
                .iter()
                .map(|(k, v)| (k.clone(), v.0.clone(), v.1))
                .collect()
        }
        fn get_bookmark(&self, name: &str) -> Option<(String, i64)> {
            self.bookmarks.get(name).cloned()
        }
        fn record_bookmark_use(&mut self, name: &str) {
            if let Some(b) = self.bookmarks.get_mut(name) {
                b.1 += 1;
            }
        }
        fn get_last_command(&self) -> Option<String> {
            self.last_command.clone()
        }
    }

    #[test]
    fn test_add_bookmark() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::new();
        let pid = getpid();
        let ctx = Context::new_safe(pid, pid, false);

        let result = add_bookmark(&ctx, "test", "echo hello", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert!(proxy.get_bookmark("test").is_some());
    }

    #[test]
    fn test_remove_bookmark() {
        use nix::unistd::getpid;
        let mut proxy = MockShellProxy::new();
        proxy.add_bookmark("test".to_string(), "echo hello".to_string());

        let pid = getpid();
        let ctx = Context::new_safe(pid, pid, false);

        let result = remove_bookmark(&ctx, "test", &mut proxy);
        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert!(proxy.get_bookmark("test").is_none());
    }
}
