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
///   out --list        - List all stored outputs
///   out --clear       - Clear output history
///   out --help        - Show help
pub fn command(_ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let args: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();

    match args.first() {
        None => {
            // Show most recent output
            show_output(proxy, 1)
        }
        Some(&"--list") | Some(&"-l") => list_outputs(proxy),
        Some(&"--clear") | Some(&"-c") => clear_outputs(proxy),
        Some(&"--help") | Some(&"-h") => {
            print_help();
            ExitStatus::ExitedWith(0)
        }
        Some(arg) => {
            // Try to parse as an index
            if let Ok(index) = arg.parse::<usize>() {
                if index == 0 {
                    eprintln!("out: index must be 1 or greater");
                    ExitStatus::ExitedWith(1)
                } else {
                    show_output(proxy, index)
                }
            } else {
                eprintln!("out: invalid argument '{}'", arg);
                eprintln!("Usage: out [N] [--list] [--clear] [--help]");
                ExitStatus::ExitedWith(1)
            }
        }
    }
}

fn show_output(proxy: &mut dyn ShellProxy, index: usize) -> ExitStatus {
    // Use get_var to access $OUT[N]
    let var_name = format!("OUT[{}]", index);
    match proxy.get_var(&var_name) {
        Some(output) => {
            if output.is_empty() {
                println!("(empty output)");
            } else {
                println!("{}", output);
            }
            ExitStatus::ExitedWith(0)
        }
        None => {
            eprintln!("out: no output at index {}", index);
            ExitStatus::ExitedWith(1)
        }
    }
}

fn list_outputs(proxy: &mut dyn ShellProxy) -> ExitStatus {
    println!();
    println!("Output History:");
    println!("─────────────────────────────────────────────────────────────────────");

    let mut found = false;
    for i in 1..=10 {
        let var_name = format!("OUT[{}]", i);
        if let Some(output) = proxy.get_var(&var_name) {
            found = true;
            let preview = output.lines().next().unwrap_or("(empty)");
            let preview = if preview.len() > 60 {
                format!("{}...", &preview[..57])
            } else {
                preview.to_string()
            };
            let lines = output.lines().count();
            let bytes = output.len();
            println!("  [{}] {} lines, {} bytes: {}", i, lines, bytes, preview);
        } else {
            break;
        }
    }

    if !found {
        println!("  No output history available.");
        println!("  Execute some commands to start collecting output.");
    }

    println!();
    ExitStatus::ExitedWith(0)
}

fn clear_outputs(proxy: &mut dyn ShellProxy) -> ExitStatus {
    // We can't directly clear from here, but we can indicate it should be cleared
    // For now, just print a message - actual clearing would require ShellProxy extension
    let _ = proxy;
    eprintln!("out: --clear is not implemented yet");
    ExitStatus::ExitedWith(1)
}

fn print_help() {
    println!("Usage: out [N] [OPTIONS]");
    println!();
    println!("Display command output history.");
    println!();
    println!("Arguments:");
    println!("  N             Show the Nth most recent output (1 = most recent)");
    println!();
    println!("Options:");
    println!("  -l, --list    List all stored outputs with previews");
    println!("  -c, --clear   Clear output history");
    println!("  -h, --help    Show this help message");
    println!();
    println!("Variables:");
    println!("  $OUT          Most recent stdout");
    println!("  $OUT[N]       Nth most recent stdout");
    println!("  $ERR          Most recent stderr");
    println!("  $ERR[N]       Nth most recent stderr");
    println!();
    println!("Examples:");
    println!("  out           Show most recent output");
    println!("  out 2         Show 2nd most recent output");
    println!("  out --list    List all stored outputs");
    println!("  echo $OUT     Use most recent output in a command");
}

/// Description for the internal print last stdout command
pub fn print_last_stdout_description() -> &'static str {
    "Internal command to print the last stdout (used for Smart Pipe)"
}

/// Internal command to print the last stdout
///
/// This is used effectively for the "Smart Pipe" feature where starting a command
/// with `|` pipes the previous output to the new command.
pub fn print_last_stdout(
    _ctx: &Context,
    _argv: Vec<String>,
    proxy: &mut dyn ShellProxy,
) -> ExitStatus {
    // Simply fetch "OUT" (which resolves to OUT[1]) and print it
    if let Some(output) = proxy.get_var("OUT") {
        print!("{}", output);
        // Ensure we end with a newline if the output didn't have one (though typically it might)
        // Actually, for piping, we should probably just print exactly what was captured.
        // But OutputEntry.stdout is a String, so it's textual.
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
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct MockShellProxy {
        vars: HashMap<String, String>,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
            }
        }
    }

    impl ShellProxy for MockShellProxy {
        fn get_var(&mut self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }

        // Stubs for other methods
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
        fn get_current_dir(&self) -> anyhow::Result<PathBuf> {
            Ok(PathBuf::from("/"))
        }
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
