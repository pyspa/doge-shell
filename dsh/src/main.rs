use crate::environment::Environment;
use crate::lisp::Value;
use crate::repl::Repl;
use crate::shell::Shell;
use anyhow::Result;
use clap::Parser;
use dsh_types::Context;
use nix::unistd::isatty;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::process::ExitCode;
use tracing::debug;

mod completion;
mod direnv;
mod dirs;
mod environment;
mod history;
mod history_import;
mod input;
mod lisp;
mod parser;
mod process;
mod prompt;
mod proxy;
mod repl;
mod shell;
mod terminal;

/// Custom error type representing normal exit
#[derive(Debug)]
pub enum ShellExit {
    Normal,
    CtrlC,
    ExitCommand,
}

impl std::fmt::Display for ShellExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellExit::Normal => write!(f, "Normal exit"),
            ShellExit::CtrlC => write!(f, "Exit by Ctrl+C"),
            ShellExit::ExitCommand => write!(f, "Exit by exit command"),
        }
    }
}

impl std::error::Error for ShellExit {}

/// Display error in a user-friendly format without stack traces
fn display_user_error(err: &anyhow::Error) {
    let error_msg = err.to_string();

    // Check if it's a command not found error
    if error_msg.contains("unknown command:") {
        if let Some(cmd_start) = error_msg.find("unknown command: ") {
            let cmd = &error_msg[cmd_start + 17..]; // Skip "unknown command: "
            eprintln!("dsh: {}: command not found", cmd.trim());
        } else {
            eprintln!("dsh: command not found");
        }
    } else if error_msg.contains("Shell terminated by double Ctrl+C")
        || error_msg.contains("Normal exit")
        || error_msg.contains("Exit by")
    {
        // Don't display normal exit messages
        debug!("Shell exiting normally: {}", error_msg);
    } else {
        // For other errors, display the root cause without debug info
        eprintln!("dsh: {error_msg}");
    }
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    command: Option<String>,

    /// Lisp script to execute
    #[arg(short, long)]
    lisp: Option<String>,

    #[command(subcommand)]
    subcommand: Option<SubCommand>,
}

#[derive(Parser)]
enum SubCommand {
    /// Import command history from another shell
    Import {
        /// Shell to import from (e.g., fish)
        shell: String,

        /// Custom path to the shell history file
        #[arg(short, long)]
        path: Option<String>,
    },
}

fn main() -> ExitCode {
    if let Err(err) = init_tracing() {
        eprintln!("Failed to initialize tracing: {err}");
        return ExitCode::FAILURE;
    }

    // Set up panic handler
    setup_panic_handler();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run_shell())
}

async fn run_shell() -> ExitCode {
    let cli = Cli::parse();

    // Handle subcommands
    if let Some(subcommand) = &cli.subcommand {
        match subcommand {
            SubCommand::Import { shell, path } => {
                return handle_import_command(shell, path.as_deref());
            }
        }
    }

    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = create_context(&shell);

    if let Some(lisp_script) = cli.lisp.as_deref() {
        execute_lisp(&mut shell, &mut ctx, lisp_script).await
    } else if let Some(command) = cli.command.as_deref() {
        execute_command(&mut shell, &mut ctx, command).await
    } else {
        run_interactive(&mut shell, &mut ctx).await
    }
}

fn handle_import_command(shell_name: &str, custom_path: Option<&str>) -> ExitCode {
    use crate::history::FrecencyHistory;
    use crate::history_import::create_importer;
    use tracing::{debug, error, info};

    debug!("Starting history import from {shell_name} shell");
    println!("Importing history from {shell_name} shell...");

    // Create a history importer for the specified shell
    let importer = match create_importer(shell_name, custom_path) {
        Ok(importer) => importer,
        Err(err) => {
            error!("Failed to create importer for {shell_name} shell: {err}");
            eprintln!("Error creating importer: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Create or load the dsh command history
    let mut history = match FrecencyHistory::from_file("dsh_cmd_history") {
        Ok(history) => history,
        Err(err) => {
            error!("Failed to load dsh command history: {err}");
            eprintln!("Error loading dsh history: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Import the history
    match importer.import(&mut history) {
        Ok(count) => {
            info!("Successfully imported {count} commands from {shell_name} shell");
            println!("Successfully imported {count} commands from {shell_name} shell.");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("Failed to import history from {shell_name} shell: {err}");
            eprintln!("Error importing history: {err}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() -> Result<()> {
    let log_file = std::sync::Arc::new(std::fs::File::create("./debug.log")?);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
    // tracing_subscriber::fmt::init();
    Ok(())
}

fn setup_panic_handler() {
    std::panic::set_hook(Box::new(|panic_info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");

        let payload = panic_info.payload().downcast_ref::<&str>().map_or_else(
            || {
                if let Some(s) = panic_info.payload().downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic payload".to_string()
                }
            },
            |s| (*s).to_string(),
        );

        // Don't show stacktrace for panics related to normal exit
        if payload.contains("Shell terminated by double Ctrl+C")
            || payload.contains("Normal exit")
            || payload.contains("Exit by")
            || payload.contains("exit command")
        {
            // Show only brief message for normal exit
            debug!("Shell exiting normally: {}", payload);
            return;
        }

        let location = panic_info.location().map_or_else(
            || "Unknown location".to_string(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );

        // Get backtrace (if RUST_BACKTRACE=1 is set)
        let backtrace = std::backtrace::Backtrace::capture();
        let backtrace_str = match backtrace.status() {
            std::backtrace::BacktraceStatus::Captured => format!("\nBacktrace:\n{backtrace}"),
            std::backtrace::BacktraceStatus::Disabled => {
                "\nBacktrace: disabled (set RUST_BACKTRACE=1 to enable)".to_string()
            }
            std::backtrace::BacktraceStatus::Unsupported => "\nBacktrace: unsupported".to_string(),
            _ => "\nBacktrace: unknown status".to_string(),
        };

        // Write directly to log file (tracing may not be initialized)
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f UTC");
        let panic_log = format!(
            "\n=== PANIC OCCURRED ===\n\
            Timestamp: {timestamp}\n\
            Thread: {thread_name}\n\
            Location: {location}\n\
            Message: {payload}{backtrace_str}\n\
            ======================\n"
        );

        // Record logs in multiple ways
        // 1. Write directly to log file
        let log_files = ["./debug.log", "./panic.log"];
        for log_file in &log_files {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
            {
                use std::io::Write;
                let _ = writeln!(file, "{panic_log}");
                let _ = file.flush();
            }
        }

        // 2. Also try to output to tracing log (if initialized)
        tracing::error!("PANIC OCCURRED: {payload} at {location}");

        // 3. Also output to stderr (maintain default behavior)
        eprintln!("\n=== doge-shell PANIC ===");
        eprintln!("Message: {payload}");
        eprintln!("Location: {location}");
        eprintln!("Thread: {thread_name}");
        eprintln!("Timestamp: {timestamp}");
        eprintln!("See debug.log and panic.log for detailed information");
        eprintln!("========================\n");
    }));
}

fn create_context(shell: &Shell) -> Context {
    // Use safe Context creation (avoid panics)
    Context::new_safe(shell.pid, shell.pgid, true)
}

async fn execute_command(shell: &mut Shell, ctx: &mut Context, command: &str) -> ExitCode {
    debug!("start shell");
    shell.set_signals();

    match shell.eval_str(ctx, command.to_string(), false).await {
        Ok(code) => {
            debug!("run command mode {:?} : {:?}", command, &code);
            code
        }
        Err(err) => {
            display_user_error(&err);
            ExitCode::FAILURE
        }
    }
}

async fn execute_lisp(shell: &mut Shell, _ctx: &mut Context, lisp_script: &str) -> ExitCode {
    debug!("Executing Lisp script: {}", lisp_script);
    shell.set_signals();

    match shell.lisp_engine.borrow().run(lisp_script) {
        Ok(value) => {
            debug!("Lisp script executed successfully: {:?}", value);
            // Print the result if it's not NIL
            if value != Value::NIL
                && let Err(err) = writeln!(std::io::stdout(), "{value}")
            {
                eprintln!("Error writing to stdout: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Error executing Lisp script: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run_interactive(shell: &mut Shell, ctx: &mut Context) -> ExitCode {
    debug!("start shell");
    shell.set_signals();
    ctx.save_history = false;

    let mut repl = Repl::new(shell);
    if let Err(err) = repl.shell.eval_str(ctx, "cd .".to_string(), false).await {
        display_user_error(&err);
        return ExitCode::FAILURE;
    }

    // Check if stdin is a terminal
    if isatty(std::io::stdin().as_raw_fd()).unwrap_or(false) {
        // Interactive mode
        debug!("Running in interactive mode");
        match repl.run_interactive().await {
            Ok(()) => ExitCode::from(0),
            Err(err) => {
                // Don't display error message for normal exit
                let err_str = err.to_string();
                if err_str.contains("Shell terminated by double Ctrl+C")
                    || err_str.contains("Normal exit")
                    || err_str.contains("Exit by")
                {
                    debug!("Shell exiting normally: {}", err_str);
                    ExitCode::from(0)
                } else {
                    display_user_error(&err);
                    ExitCode::FAILURE
                }
            }
        }
    } else {
        // Pipe mode - read from stdin
        debug!("Running in pipe mode");
        let stdin = io::stdin();
        let reader = BufReader::new(stdin);

        for line in reader.lines() {
            match line {
                Ok(input) => {
                    let input = input.trim();
                    if input.is_empty() {
                        continue;
                    }
                    if input == "exit" {
                        break;
                    }
                    debug!("Processing pipe input: {}", input);
                    match repl.shell.eval_str(ctx, input.to_string(), false).await {
                        Ok(_) => {}
                        Err(err) => {
                            eprint!("Error executing '{input}': ");
                            display_user_error(&err);
                        }
                    }
                }
                Err(err) => {
                    eprintln!("Error reading input: {err}");
                    break;
                }
            }
        }
        ExitCode::from(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    #[test]
    #[ignore] // Ignore in normal test runs (for manual execution)
    fn test_panic_handler() {
        // Use test log files
        let test_log_files = ["./debug.log", "./panic.log"];

        // Remove existing log files
        for log_file in &test_log_files {
            let _ = fs::remove_file(log_file);
        }

        // Set up panic handler
        setup_panic_handler();

        // Trigger panic in separate thread
        let handle = thread::spawn(|| {
            panic!("Test panic for logging verification");
        });

        // Wait for panic
        let _ = handle.join();

        // Wait a bit then check log files
        thread::sleep(Duration::from_millis(200));

        // Check if log files are created and panic info is recorded
        let mut found_panic_log = false;
        for log_file in &test_log_files {
            if let Ok(content) = fs::read_to_string(log_file)
                && content.contains("PANIC OCCURRED")
                && content.contains("Test panic for logging verification")
            {
                found_panic_log = true;
                println!("Panic information found in {log_file}");
                break;
            }
        }

        assert!(
            found_panic_log,
            "Panic information not found in any log file"
        );
        println!("Panic handler test passed - check debug.log and panic.log for details");
    }
}
