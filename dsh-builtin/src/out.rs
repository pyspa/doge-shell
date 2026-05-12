//! Output history builtin command
//!
//! Provides the `out` builtin command for viewing command output history.

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in out command description
pub fn description() -> &'static str {
    "Display command output history"
}

/// Built-in out command implementation
///
/// Usage:
///   out               - Show the most recent command output
///   out N             - Show the Nth most recent output (1 = most recent)
///   out --list        - List stored outputs
///   out --list --limit N
///   out --clear       - Clear output history
///   out --help        - Show help
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let options = match parse_options(&argv[1..]) {
        Ok(options) => options,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("out: {err}"));
            let _ = ctx.write_stderr("Usage: out [N] [--list] [--limit N] [--clear] [--help]");
            return ExitStatus::ExitedWith(1);
        }
    };

    match options.mode {
        OutMode::Show(index) => show_output(ctx, proxy, index),
        OutMode::List => list_outputs(ctx, proxy, options.limit),
        OutMode::Clear => clear_outputs(ctx, proxy),
        OutMode::Help => {
            let _ = ctx.write_stdout(help_text());
            ExitStatus::ExitedWith(0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutMode {
    Show(usize),
    List,
    Clear,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutOptions {
    mode: OutMode,
    limit: usize,
}

fn parse_options(args: &[String]) -> Result<OutOptions, String> {
    let mut mode = None;
    let mut limit = 10;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" | "help" => set_mode(&mut mode, OutMode::Help)?,
            "-l" | "--list" | "list" => set_mode(&mut mode, OutMode::List)?,
            "-c" | "--clear" | "clear" => set_mode(&mut mode, OutMode::Clear)?,
            "-n" | "--limit" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--limit requires a number".to_string());
                };
                limit = parse_positive_usize(value, "limit")?;
            }
            value if value.starts_with("--limit=") => {
                let value = value.trim_start_matches("--limit=");
                limit = parse_positive_usize(value, "limit")?;
            }
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            value => {
                let show_index = parse_positive_usize(value, "index")?;
                set_mode(&mut mode, OutMode::Show(show_index))?;
            }
        }
        index += 1;
    }

    let mode = mode.unwrap_or(OutMode::Show(1));
    if !matches!(mode, OutMode::List) && limit != 10 {
        return Err("--limit can only be used with --list".to_string());
    }

    Ok(OutOptions { mode, limit })
}

fn set_mode(mode: &mut Option<OutMode>, next: OutMode) -> Result<(), String> {
    if mode.replace(next).is_some() {
        return Err("only one of index, --list, --clear, or --help can be used".to_string());
    }
    Ok(())
}

fn parse_positive_usize(value: &str, label: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{label} must be a number"))?;
    if parsed == 0 {
        return Err(format!("{label} must be 1 or greater"));
    }
    Ok(parsed)
}

fn show_output(ctx: &Context, proxy: &mut dyn ShellProxy, index: usize) -> ExitStatus {
    let var_name = format!("OUT[{}]", index);
    match proxy.get_var(&var_name) {
        Some(output) => {
            if output.is_empty() {
                let _ = ctx.write_stdout("(empty output)");
            } else {
                let _ = ctx.write_stdout(&output);
            }
            ExitStatus::ExitedWith(0)
        }
        None => {
            let _ = ctx.write_stderr(&format!("out: no output at index {}", index));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn list_outputs(ctx: &Context, proxy: &mut dyn ShellProxy, limit: usize) -> ExitStatus {
    let history = proxy.get_full_output_history();

    if history.is_empty() {
        let _ = ctx.write_stdout(
            "No output history available.\nExecute commands with captured output to populate it.",
        );
        return ExitStatus::ExitedWith(0);
    }

    let mut lines = vec![
        "Output History:".to_string(),
        "Index  Exit  Lines  Bytes  Command / Preview".to_string(),
        "-----  ----  -----  -----  -----------------".to_string(),
    ];

    for (offset, entry) in history.into_iter().take(limit).enumerate() {
        let output = if entry.stdout.is_empty() {
            entry.stderr.as_str()
        } else {
            entry.stdout.as_str()
        };
        let preview = preview_line(output, 72);
        let line_count = output.lines().count();
        let bytes = output.len();
        let suffix = if preview.is_empty() {
            String::new()
        } else {
            format!(" -- {preview}")
        };
        lines.push(format!(
            "{:>5}  {:>4}  {:>5}  {:>5}  {}{}",
            offset + 1,
            entry.exit_code,
            line_count,
            bytes,
            entry.command,
            suffix
        ));
    }

    let _ = ctx.write_stdout(&lines.join("\n"));
    ExitStatus::ExitedWith(0)
}

fn preview_line(output: &str, max_chars: usize) -> String {
    let first = output.lines().next().unwrap_or("").trim();
    let mut preview = String::new();
    for ch in first.chars().take(max_chars) {
        preview.push(ch);
    }
    if first.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn clear_outputs(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let removed = proxy.clear_output_history();
    let _ = ctx.write_stdout(&format!("Cleared {removed} output history entries."));
    ExitStatus::ExitedWith(0)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: out [N] [OPTIONS]\n",
        "\n",
        "Display command output history. Use `tm` for interactive fuzzy search with preview.\n",
        "\n",
        "Arguments:\n",
        "  N                  Show the Nth most recent output (1 = most recent)\n",
        "\n",
        "Options:\n",
        "  -l, --list         List stored outputs with previews\n",
        "  -n, --limit N      Limit list output (default: 10)\n",
        "  -c, --clear        Clear output history\n",
        "  -h, --help         Show this help message\n",
        "\n",
        "Variables:\n",
        "  $OUT               Most recent stdout\n",
        "  $OUT[N]            Nth most recent stdout\n",
        "  $ERR               Most recent stderr\n",
        "  $ERR[N]            Nth most recent stderr\n",
        "\n",
        "Examples:\n",
        "  out\n",
        "  out 2\n",
        "  out --list --limit 25\n",
        "  out --clear\n",
    )
}

/// Description for the internal print last stdout command
pub fn print_last_stdout_description() -> &'static str {
    "Internal command to print the last stdout (used for Smart Pipe)"
}

/// Internal command to print the last stdout
///
/// This is used for the Smart Pipe feature where starting a command with `|`
/// pipes the previous output to the new command.
pub fn print_last_stdout(
    _ctx: &Context,
    _argv: Vec<String>,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    if let Some(output) = proxy.get_var("OUT") {
        print!("{}", output);
        if !output.ends_with('\n') {
            println!();
        }
    }
    ExitStatus::ExitedWith(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::mcp::McpServerConfig;
    use dsh_types::output_history::OutputEntry;
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct MockShellProxy {
        vars: HashMap<String, String>,
        history: Vec<OutputEntry>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
                history: Vec::new(),
            }
        }
    }

    impl ShellProxy for MockShellProxy {
        fn get_var(&mut self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
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
        fn set_var(&mut self, key: String, value: String) {
            self.vars.insert(key, value);
        }
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn unset_env_var(&mut self, _key: &str) {}
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
        fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn export_var(&mut self, _key: &str) -> bool {
            false
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
        fn get_current_dir(&self) -> anyhow::Result<PathBuf> {
            Ok(PathBuf::from("/"))
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
        fn get_full_output_history(&self) -> Vec<OutputEntry> {
            self.history.clone()
        }
        fn clear_output_history(&mut self) -> usize {
            let removed = self.history.len();
            self.history.clear();
            removed
        }
    }

    #[test]
    fn parse_options_supports_list_limit_and_clear() {
        let args = vec![
            "--list".to_string(),
            "--limit".to_string(),
            "25".to_string(),
        ];
        assert_eq!(
            parse_options(&args).unwrap(),
            OutOptions {
                mode: OutMode::List,
                limit: 25
            }
        );

        let args = vec!["--clear".to_string()];
        assert_eq!(parse_options(&args).unwrap().mode, OutMode::Clear);
    }

    #[test]
    fn parse_options_rejects_limit_without_list() {
        let args = vec!["2".to_string(), "--limit".to_string(), "3".to_string()];
        assert!(parse_options(&args).is_err());
    }

    #[test]
    fn preview_line_truncates_on_char_boundaries() {
        assert_eq!(preview_line("abcdef", 3), "abc...");
        assert_eq!(preview_line("あいうえお", 3), "あいう...");
    }

    #[test]
    fn test_print_last_stdout() {
        use nix::unistd::Pid;
        let mut proxy = MockShellProxy::new();
        proxy
            .vars
            .insert("OUT".to_string(), "hello world".to_string());

        let ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true);
        let status = print_last_stdout(&ctx, vec![], &mut proxy);

        assert_eq!(status, ExitStatus::ExitedWith(0));
    }
}
