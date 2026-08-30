use crate::environment::Environment;
use crate::lisp::Value;
use crate::repl::Repl;
use crate::shell::Shell;
use anyhow::Result;
use clap::Parser;
use dsh_types::Context;
use nix::unistd::isatty;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tracing::debug;

pub mod ai_features;
pub mod argument_explainer;
pub mod blocks_ui;
pub mod command_palette;
pub mod command_suggestion;
pub mod command_timing;
pub mod completion;
pub mod db;
pub mod direnv;
pub mod dirs;
pub mod environment;
pub mod errors;
pub mod github;
pub mod history;
pub mod history_import;
pub mod input;
pub mod lisp;
// pub mod notebook;
pub mod output_schema;
pub mod parser;
#[doc(hidden)]
pub mod perf_probes;
pub mod process;
pub mod prompt;
pub mod proxy;
pub mod repl;
pub mod safety;
pub mod scheduler;
pub mod secrets;
pub mod shell;
pub mod snippet;
pub mod suggestion;
pub mod terminal;
pub mod utils;

use crate::errors::display_user_error;

#[cfg(test)]
pub(crate) fn test_env_lock() -> parking_lot::MutexGuard<'static, ()> {
    static LOCK: std::sync::LazyLock<parking_lot::Mutex<()>> =
        std::sync::LazyLock::new(|| parking_lot::Mutex::new(()));
    LOCK.lock()
}

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

    /// Generate AI-powered completion definition for a command
    Completion {
        /// Command to generate completion for
        command: String,

        /// Output file path (default: ~/.config/dsh/completions/<command>.json)
        #[arg(short, long)]
        output: Option<String>,

        /// Force overwrite existing completion file
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunMode {
    Interactive,
    Command(String),
    Lisp(String),
    Notebook(PathBuf),
}

impl RunMode {
    fn from_cli(cli: &Cli) -> Self {
        if let Some(script) = &cli.lisp {
            Self::Lisp(script.clone())
        } else if let Some(command) = &cli.command {
            Self::Command(command.clone())
        } else if let Some(path) = &cli.notebook {
            Self::Notebook(PathBuf::from(path))
        } else {
            Self::Interactive
        }
    }

    fn needs_interactive_services(&self) -> bool {
        matches!(self, Self::Interactive | Self::Notebook(_))
    }
}

/// Owns work that is useful only while an interactive shell is alive.
///
/// Short-lived filesystem loader threads are joined when they have already
/// completed and are otherwise detached so shutdown never waits on filesystem
/// I/O. In particular, these tasks must not run on Tokio's blocking pool,
/// because runtime shutdown waits for started `spawn_blocking` work.
#[derive(Default)]
struct StartupBackgroundTasks {
    loader_threads: Vec<std::thread::JoinHandle<()>>,
}

impl StartupBackgroundTasks {
    fn push_loader(&mut self, thread: std::thread::JoinHandle<()>) {
        self.loader_threads.push(thread);
    }
}

impl Drop for StartupBackgroundTasks {
    fn drop(&mut self) {
        for thread in self.loader_threads.drain(..) {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

pub fn lib_main() -> ExitCode {
    if let Err(err) = init_tracing() {
        eprintln!("Failed to initialize tracing: {err}");
        return ExitCode::FAILURE;
    }

    // Set up panic handler
    setup_panic_handler();

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("Failed to create Tokio runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
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
            SubCommand::Completion {
                command,
                output,
                force,
            } => {
                return handle_completion_command(command.clone(), output.clone(), *force).await;
            }
        }
    }

    let run_mode = RunMode::from_cli(&cli);
    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut startup_tasks = StartupBackgroundTasks::default();

    if run_mode.needs_interactive_services() {
        // Initialize command history (Async)
        let cmd_history =
            std::sync::Arc::new(parking_lot::Mutex::new(crate::history::History::new()));
        shell.cmd_history = Some(cmd_history.clone());

        startup_tasks.push_loader(std::thread::spawn(move || {
            match crate::history::History::from_file("dsh_cmd_history") {
                Ok(mut history) => {
                    // Preload recent history (fast, immediate)
                    let min_timestamp = match history.load_recent(1000) {
                        Ok(ts) => ts,
                        Err(e) => {
                            tracing::warn!("Failed to load recent history items: {}", e);
                            0
                        }
                    };
                    history.start_background_writer();

                    // Swap shared history immediately so user has something
                    {
                        *cmd_history.lock() = history.clone();
                    }

                    // Load the rest of history in background (slower)
                    if min_timestamp > 0 {
                        match history.load_older_than(min_timestamp, 9000) {
                            Ok(entries) => {
                                if !entries.is_empty() {
                                    let mut locked = cmd_history.lock();
                                    locked.prepend(entries);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to load remaining history: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to load command history: {}", e);
                }
            }
        }));

        // Initialize directory history (Async)
        let path_history = std::sync::Arc::new(parking_lot::Mutex::new(
            crate::history::FrecencyHistory::new(),
        ));
        shell.path_history = Some(path_history.clone());

        startup_tasks.push_loader(std::thread::spawn(move || {
            match crate::history::FrecencyHistory::from_file("dsh_directory_history") {
                Ok(mut history) => {
                    history.start_background_writer();
                    *path_history.lock() = history;
                }
                Err(e) => {
                    tracing::warn!("Failed to load directory history: {}", e);
                }
            }
        }));
    }

    // Notebook setup remains independent from the execution mode for CLI
    // compatibility: `--notebook file -c ...` and `--notebook file -l ...`
    // historically opened the notebook before running the one-shot command.
    if let Some(notebook_path) = cli.notebook.as_deref() {
        if let Err(e) = shell.open_notebook(PathBuf::from(notebook_path)) {
            tracing::error!("Failed to open notebook: {}", e);
            eprintln!("Error opening notebook: {}", e);
            // Decide whether to continue or exit. Continuing without notebook mode is safer but warning is needed.
        } else {
            println!("Notebook Mode Active.");
        }
    }

    // Load config.lisp to initialize aliases, variables, and other settings
    // Enable startup mode to prevent blocking MCP server connections
    shell.environment.write().startup_mode = true;
    if let Err(e) = shell.lisp_engine.borrow().run_config_lisp() {
        // Only warn if it's not a "file not found" error (config.lisp is optional)
        let err_str = e.to_string();
        if !err_str.contains("No such file or directory") && !err_str.contains("config file") {
            tracing::warn!("Failed to load config.lisp: {}", e);
            eprintln!("Warning: Failed to load config.lisp: {}", e);
        }
    }
    // Disable startup mode
    shell.environment.write().startup_mode = false;

    if run_mode.needs_interactive_services() {
        // MCP connections and executable discovery are interactive services.
        shell.reload_mcp_config();

        // Prewarm executable names cache in background for faster command prefix search.
        let paths = shell.environment.read().variable_state.paths.clone();
        let names_arc =
            std::sync::Arc::clone(&shell.environment.read().completion_state.executable_names);
        if let Some(names) = crate::environment::load_cached_executables(&paths) {
            *names_arc.write() = names.clone();
            let set: std::collections::BTreeSet<String> = names.into_iter().collect();
            crate::completion::generator::set_global_system_commands(set);
        }
        let prewarm_thread = std::thread::Builder::new()
            .name("dsh-executable-prewarm".to_string())
            .spawn(move || {
                let names = crate::environment::collect_executables(&paths);
                let _ = crate::environment::save_cached_executables(&paths, &names);
                *names_arc.write() = names;
                let set: std::collections::BTreeSet<String> =
                    names_arc.read().iter().cloned().collect();
                crate::completion::generator::set_global_system_commands(set);
            });
        match prewarm_thread {
            Ok(thread) => startup_tasks.push_loader(thread),
            Err(err) => {
                tracing::warn!("Failed to start executable prewarm thread: {err}");
            }
        }
    }

    let mut ctx = create_context(&shell);

    match run_mode {
        RunMode::Lisp(script) => execute_lisp(&mut shell, &mut ctx, &script).await,
        RunMode::Command(command) => execute_command(&mut shell, &mut ctx, &command).await,
        RunMode::Interactive | RunMode::Notebook(_) => run_interactive(&mut shell, &mut ctx).await,
    }
}

pub async fn handle_completion_command(
    command: String,
    output: Option<String>,
    force: bool,
) -> ExitCode {
    use crate::ai_features::generate_completion_json;
    use crate::environment::Environment;
    use dsh_builtin::completion_generation::CompletionGenerationService;
    use dsh_openai::{ChatGptClient, OpenAiConfig};
    use std::path::PathBuf;
    use tracing::{debug, error, info};

    info!("Generating completion for command: {}", command);

    let help_text = match CompletionGenerationService::collect_help_text(&command) {
        Ok(help_text) => help_text,
        Err(e) => {
            error!("Failed to collect help for '{}': {:#}", command, e);
            eprintln!("Error: Failed to get help text for '{}': {}", command, e);
            return ExitCode::FAILURE;
        }
    };

    debug!("Got help text ({} chars)", help_text.len());

    // Initialize AI service
    let env = Environment::new();
    let config = OpenAiConfig::from_getter(|key| {
        let value = {
            let guard = env.read();
            guard.get_var(key)
        };
        value.or_else(|| std::env::var(key).ok())
    });

    let _api_key = match config.api_key() {
        Some(key) => key,
        None => {
            error!("OpenAI API key not configured. Set OPENAI_API_KEY environment variable.");
            eprintln!(
                "Error: OpenAI API key not configured. Set OPENAI_API_KEY environment variable."
            );
            return ExitCode::FAILURE;
        }
    };

    let client = match ChatGptClient::try_from_config(&config) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create AI client: {}", e);
            eprintln!("Error: Failed to create AI client: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let mcp_manager = env.read().integration_state.mcp_manager.clone();
    let safety_level = env.read().policy_state.safety_level.clone();
    let allowlist = env.read().policy_state.execute_allowlist.clone();
    let safety_guard = Arc::new(crate::safety::SafetyGuard::new());

    let service = crate::ai_features::LiveAiService::new(
        client,
        mcp_manager,
        safety_level,
        safety_guard,
        None,
        allowlist,
    );

    // Generate completion JSON using AI
    let completion_json = match generate_completion_json(&service, &command, &help_text).await {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to generate completion JSON: {}", e);
            eprintln!("Error: Failed to generate completion: {}", e);
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = CompletionGenerationService::validate_json(&completion_json, &command) {
        error!("Generated completion failed validation: {:#}", e);
        eprintln!("Error: Generated completion failed validation: {e:#}");
        return ExitCode::FAILURE;
    }

    let output_path = match output {
        Some(path) => PathBuf::from(path),
        None => match CompletionGenerationService::default_output_path(&command) {
            Ok(path) => path,
            Err(e) => {
                error!("Failed to resolve completion output path: {:#}", e);
                eprintln!("Error: Failed to resolve completion output path: {e:#}");
                return ExitCode::FAILURE;
            }
        },
    };

    match CompletionGenerationService::write_json_atomic(
        &output_path,
        &completion_json,
        &command,
        force,
    ) {
        Ok(()) => {}
        Err(e) => {
            error!("Failed to write completion file: {:#}", e);
            eprintln!("Error: Failed to write completion file: {e:#}");
            return ExitCode::FAILURE;
        }
    }

    info!("Completion written to: {}", output_path.display());
    println!(
        "Completion generated and saved to: {}",
        output_path.display()
    );
    ExitCode::SUCCESS
}

pub fn handle_import_command(shell_name: &str, custom_path: Option<&str>) -> ExitCode {
    use crate::history::History;
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
    let mut history = match History::from_file("dsh_cmd_history") {
        Ok(h) => h,
        Err(err) => {
            error!("Failed to open dsh command history database: {err}");
            eprintln!("Error opening dsh history database: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = history.load() {
        error!("Failed to load dsh command history: {err}");
        eprintln!("Error loading dsh history: {err}");
        return ExitCode::FAILURE;
    }

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
    let log_path = crate::environment::get_state_file("debug.log")
        .unwrap_or_else(|_| std::path::PathBuf::from("./debug.log"));

    let log_file = std::sync::Arc::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?,
    );

    let env_filter = tracing_subscriber::EnvFilter::try_from_env("DSH_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(env_filter)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
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
        let log_files = ["debug.log", "panic.log"];
        for log_name in &log_files {
            let log_path = crate::environment::get_state_file(log_name)
                .unwrap_or_else(|_| std::path::PathBuf::from(log_name));

            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
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
        eprintln!("See log files in state directory for detailed information");
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

    let stdin_is_tty = isatty(unsafe { BorrowedFd::borrow_raw(stdin_fd) }).unwrap_or(false);
    let stdout_is_tty = isatty(unsafe { BorrowedFd::borrow_raw(stdout_fd) }).unwrap_or(false);

    // Create a basic terminal state based on whether file descriptors are TTYs
    let terminal_state = if stdin_is_tty {
        // If stdin is a TTY, try to get its terminal settings
        match tcgetattr(unsafe { BorrowedFd::borrow_raw(stdin_fd) }) {
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
        match tcgetattr(unsafe { BorrowedFd::borrow_raw(stdin_fd) })
            .or_else(|_| tcgetattr(unsafe { BorrowedFd::borrow_raw(stdout_fd) }))
            .or_else(|_| tcgetattr(unsafe { BorrowedFd::borrow_raw(stderr_fd) }))
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
        shell_tmode: Some(shell_tmode),
        terminal_state: terminal_state.clone(),
        shell_mode,
        foreground: false, // For command execution, not foreground
        interactive: terminal_state.is_terminal,
        infile: stdin_fd,
        outfile: stdout_fd,
        errfile: stderr_fd,
        captured_out: None,
        output_observer: None,
        save_history: true,
        pid: None,
        pgid: None,
        process_count: 0,
    }
}

pub async fn execute_command(shell: &mut Shell, _ctx: &mut Context, command: &str) -> ExitCode {
    debug!("start shell");
    shell.set_signals();

    // Initialize AI service for non-interactive commands
    {
        use dsh_openai::ChatGptClient;
        use dsh_openai::OpenAiConfig;
        use std::sync::Arc;

        let env_handle = Arc::clone(&shell.environment);
        let config = OpenAiConfig::from_getter(|key| {
            let value = {
                let guard = env_handle.read();
                guard.get_var(key)
            };
            value.or_else(|| std::env::var(key).ok())
        });

        // Only initialize if API key is present
        if config.api_key().is_some()
            && let Ok(client) = ChatGptClient::try_from_config(&config)
        {
            let mcp_manager = env_handle.read().integration_state.mcp_manager.clone();
            let safety_level = env_handle.read().policy_state.safety_level.clone();
            let allowlist = env_handle.read().policy_state.execute_allowlist.clone();
            let service = Arc::new(crate::ai_features::LiveAiService::new(
                client,
                mcp_manager,
                safety_level,
                shell.safety_guard.clone(),
                None,
                allowlist,
            ));
            shell.environment.write().integration_state.ai_service = Some(service);
        }
    }

    // For command execution, we create a special context that doesn't require full TTY access
    // This avoids the /dev/tty access issue in test environments
    let mut ctx = create_context_for_command(shell);

    // In command mode, we may not have interactive features available
    // Set appropriate context flags for non-interactive execution
    ctx.interactive = false;

    match shell.eval_str(&mut ctx, command.to_string(), false).await {
        Ok(code) => {
            shell.record_history_outcome(command, code, std::time::Duration::from_millis(0), None);
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
    if isatty(unsafe { BorrowedFd::borrow_raw(std::io::stdin().as_raw_fd()) }).unwrap_or(false) {
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
    fn run_mode_limits_interactive_services_to_interactive_and_notebook() {
        let base = Cli {
            command: None,
            lisp: None,
            notebook: None,
            subcommand: None,
        };
        assert!(RunMode::from_cli(&base).needs_interactive_services());

        let notebook = Cli {
            notebook: Some("session.md".to_string()),
            ..base
        };
        assert!(RunMode::from_cli(&notebook).needs_interactive_services());

        let command = Cli {
            command: Some("true".to_string()),
            notebook: None,
            ..notebook
        };
        assert!(!RunMode::from_cli(&command).needs_interactive_services());

        let lisp = Cli {
            command: None,
            lisp: Some("(+ 1 2)".to_string()),
            ..command
        };
        assert!(!RunMode::from_cli(&lisp).needs_interactive_services());
    }

    #[test]
    fn startup_background_tasks_do_not_wait_for_running_loader_threads() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let mut tasks = StartupBackgroundTasks::default();
        tasks.push_loader(std::thread::spawn(move || {
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        }));

        let started = std::time::Instant::now();
        drop(tasks);
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "dropping startup tasks waited for a running filesystem loader"
        );
        let _ = release_tx.send(());
    }

    #[test]
    #[ignore] // Ignore in normal test runs (for manual execution)
    fn test_panic_handler() {
        // Use test log files
        let test_log_files = ["debug.log", "panic.log"]
            .iter()
            .map(|name| crate::environment::get_state_file(name).unwrap())
            .collect::<Vec<_>>();

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
                println!("Panic information found in {:?}", log_file);
                break;
            }
        }

        assert!(
            found_panic_log,
            "Panic information not found in any log file"
        );
        println!("Panic handler test passed - check log files for details");
    }
}
