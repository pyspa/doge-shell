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
