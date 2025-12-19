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

pub mod ai_features;
pub mod command_palette;
pub mod command_suggestion;
pub mod command_timing;
pub mod completion;
pub mod direnv;
pub mod dirs;
pub mod environment;
pub mod errors;
pub mod github;
pub mod history;
pub mod history_import;
pub mod input;
pub mod lisp;
pub mod notebook;
pub mod output_history;
pub mod parser;
pub mod process;
pub mod prompt;
pub mod proxy;
pub mod repl;
pub mod safety;
pub mod shell;
pub mod suggestion;
pub mod terminal;

use crate::errors::display_user_error;

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
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[arg(short, long)]
    pub command: Option<String>,

    /// Lisp script to execute
    #[arg(short, long)]
    pub lisp: Option<String>,

    /// Open in Notebook mode with the specified file
    #[arg(long)]
    pub notebook: Option<String>,

    #[command(subcommand)]
    pub subcommand: Option<SubCommand>,
}

#[derive(Parser)]
pub enum SubCommand {
    /// Import command history from another shell
    Import {
        /// Shell to import from (e.g., fish)
        shell: String,

        /// Custom path to the shell history file
        #[arg(short, long)]
        path: Option<String>,
    },
}

pub fn lib_main() -> ExitCode {
    if let Err(err) = init_tracing() {
        eprintln!("Failed to initialize tracing: {err}");
        return ExitCode::FAILURE;
    }

    // Set up panic handler
    setup_panic_handler();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run_shell())
}

pub async fn run_shell() -> ExitCode {
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

    // Initialize command history
    match crate::history::FrecencyHistory::from_file("dsh_cmd_history") {
        Ok(history) => {
            shell.cmd_history = Some(std::sync::Arc::new(parking_lot::Mutex::new(history)));
        }
        Err(e) => {
            tracing::warn!("Failed to load command history: {}", e);
        }
    }

    // Initialize directory history
    match crate::history::FrecencyHistory::from_file("dsh_directory_history") {
        Ok(history) => {
            shell.path_history = Some(std::sync::Arc::new(parking_lot::Mutex::new(history)));
        }
        Err(e) => {
            tracing::warn!("Failed to load directory history: {}", e);
        }
    }

    // Initialize Notebook Mode if requested
    if let Some(notebook_path) = cli.notebook {
        if let Err(e) = shell.open_notebook(std::path::PathBuf::from(notebook_path)) {
            tracing::error!("Failed to open notebook: {}", e);
            eprintln!("Error opening notebook: {}", e);
            // Decide whether to continue or exit. Continuing without notebook mode is safer but warning is needed.
        } else {
            println!("Notebook Mode Active.");
        }
    }

    // Load config.lisp to initialize aliases, variables, and other settings
    if let Err(e) = shell.lisp_engine.borrow().run_config_lisp() {
        // Only warn if it's not a "file not found" error (config.lisp is optional)
        let err_str = e.to_string();
        if !err_str.contains("No such file or directory") && !err_str.contains("config file") {
            tracing::warn!("Failed to load config.lisp: {}", e);
        }
    }

    let mut ctx = create_context(&shell);

    if let Some(lisp_script) = cli.lisp.as_deref() {
        execute_lisp(&mut shell, &mut ctx, lisp_script).await
    } else if let Some(command) = cli.command.as_deref() {
        execute_command(&mut shell, &mut ctx, command).await
    } else {
        run_interactive(&mut shell, &mut ctx).await
    }
}

pub fn handle_import_command(shell_name: &str, custom_path: Option<&str>) -> ExitCode {
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

pub fn init_tracing() -> Result<()> {
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

pub fn setup_panic_handler() {
    std::panic::set_hook(Box::new(|panic_info| {
        // Attempt to restore terminal state first
        let _ = crossterm::terminal::disable_raw_mode();

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

pub fn create_context(shell: &Shell) -> Context {
    // Use safe Context creation (avoid panics)
    Context::new_safe(shell.pid, shell.pgid, true)
}

// Create a function to create context with different settings for non-interactive mode
pub fn create_context_for_command(shell: &Shell) -> Context {
    // For command mode execution, use a minimal context that doesn't require full TTY access
    use dsh_types::terminal::{ShellMode, TerminalState};
    use nix::sys::termios::tcgetattr;
    use nix::unistd::isatty;
    use std::os::unix::io::AsRawFd;

    let stdin_fd = std::io::stdin().as_raw_fd();
    let stdout_fd = std::io::stdout().as_raw_fd();
    let stderr_fd = std::io::stderr().as_raw_fd();

    let stdin_is_tty = isatty(stdin_fd).unwrap_or(false);
    let stdout_is_tty = isatty(stdout_fd).unwrap_or(false);

    // Create a basic terminal state based on whether file descriptors are TTYs
    let terminal_state = if stdin_is_tty {
        // If stdin is a TTY, try to get its terminal settings
        match tcgetattr(stdin_fd) {
            Ok(tmodes) => TerminalState {
                is_terminal: true,
                tmodes: Some(tmodes),
                supports_job_control: true,
            },
            Err(_) => {
                // If we can't get terminal settings from stdin, create a basic one
                // This will be the case in some test environments
                TerminalState {
                    is_terminal: false,
                    tmodes: None,
                    supports_job_control: false,
                }
            }
        }
    } else {
        // Stdin is not a TTY, create a non-terminal state
        TerminalState::non_terminal()
    };

    let shell_mode = if stdin_is_tty && stdout_is_tty {
        ShellMode::Interactive
    } else if stdin_is_tty && !stdout_is_tty {
        ShellMode::Pipeline
    } else {
        ShellMode::Script
    };

    // For command execution in test environments, try to get Termios from any available file descriptor
    // that's a TTY. If none are available, fall back to new_safe which doesn't require TTY.
    let shell_tmode = if let Some(tmodes) = &terminal_state.tmodes {
        tmodes.clone()
    } else {
        // Try to get terminal settings from any standard file descriptor that might be a TTY
        match tcgetattr(stdin_fd)
            .or_else(|_| tcgetattr(stdout_fd))
            .or_else(|_| tcgetattr(stderr_fd))
        {
            Ok(tmodes) => tmodes,
            Err(_) => {
                // For environments where no TTY is available (test environments, pipes, etc.),
                // try /dev/tty as a last resort
                use nix::fcntl::{OFlag, open};
                use nix::sys::stat::Mode;

                match open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
                    .ok()
                    .and_then(|tty_fd| tcgetattr(tty_fd).ok())
                {
                    Some(tmodes) => tmodes,
                    None => {
                        // No TTY available at all - use Context::new_safe which handles this
                        debug!("No TTY available for command execution, using safe context");
                        return Context::new_safe(shell.pid, shell.pgid, false);
                    }
                }
            }
        }
    };

    Context {
        shell_pid: shell.pid,
        shell_pgid: shell.pgid,
        shell_tmode,
        terminal_state: terminal_state.clone(),
        shell_mode,
        foreground: false, // For command execution, not foreground
        interactive: terminal_state.is_terminal,
        infile: stdin_fd,
        outfile: stdout_fd,
        errfile: stderr_fd,
        captured_out: None,
        save_history: true,
        pid: None,
        pgid: None,
        process_count: 0,
    }
}

pub async fn execute_command(shell: &mut Shell, _ctx: &mut Context, command: &str) -> ExitCode {
    debug!("start shell");
    shell.set_signals();

    // For command execution, we create a special context that doesn't require full TTY access
    // This avoids the /dev/tty access issue in test environments
    let mut ctx = create_context_for_command(shell);

    // In command mode, we may not have interactive features available
    // Set appropriate context flags for non-interactive execution
    ctx.interactive = false;

    match shell.eval_str(&mut ctx, command.to_string(), false).await {
        Ok(code) => {
            debug!("run command mode {:?} : {:?}", command, &code);
            ExitCode::from(code.clamp(0, 255) as u8)
        }
        Err(err) => {
            display_user_error(&err, true);
            ExitCode::FAILURE
        }
    }
}

pub async fn execute_lisp(shell: &mut Shell, _ctx: &mut Context, lisp_script: &str) -> ExitCode {
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

pub async fn run_interactive(shell: &mut Shell, ctx: &mut Context) -> ExitCode {
    debug!("start shell");
    shell.set_signals();
    ctx.save_history = false;

    let mut repl = Repl::new(shell);
    if let Err(err) = repl.shell.eval_str(ctx, "cd .".to_string(), false).await {
        display_user_error(&err, true);
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
                    display_user_error(&err, true);
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
                            display_user_error(&err, true);
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
