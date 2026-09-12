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
    use crate::test_support::TestShellProxy;

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
        let mut proxy = TestShellProxy::default();
        proxy
            .vars
            .insert("OUT".to_string(), "hello world".to_string());

        let ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true);
        let status = print_last_stdout(&ctx, vec![], &mut proxy);

        assert_eq!(status, ExitStatus::ExitedWith(0));
    }
}
