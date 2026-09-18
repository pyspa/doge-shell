//! The four ways a `dsh` process can run: interactive REPL, `-c <command>`,
//! `-l <lisp>`, and the subcommand dispatch in `run_shell` that precedes all
//! of them. `Context` construction (`create_context`/`create_context_for_command`)
//! lives here too since each run mode builds its own.
use crate::agent_lifecycle;
use crate::bootstrap::{StartupBackgroundTasks, spawn_herdr_shutdown_signal_watcher};
use crate::cli::{Cli, RunMode, SubCommand};
use crate::environment::Environment;
use crate::errors::display_user_error;
use crate::lisp::Value;
use crate::repl::Repl;
use crate::shell::Shell;
use crate::subcommands::{handle_completion_command, handle_import_command};
use clap::Parser;
use dsh_types::Context;
use nix::unistd::isatty;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::debug;

pub async fn run_shell() -> ExitCode {
    let cli = Cli::parse();

    // Internal re-exec helper (background builtins, isolated subshells).
    // Runs before everything: no config.lisp, no history, no MCP, no
    // notebook, no lifecycle activation. The helper is an execution detail
    // of the parent session, not a new interactive agent.
    if let Some(exec_fd) = cli.internal_exec_fd {
        return crate::process::reexec::run_internal_helper(exec_fd).await;
    }

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

    // Covers all three modes, including `-c`, which a `!` chat can reach.
    let _chat_jobs_shutdown = ChatJobsShutdown;

    match run_mode {
        RunMode::Lisp(script) => execute_lisp(&mut shell, &mut ctx, &script).await,
        RunMode::Command(command) => execute_command(&mut shell, &mut ctx, &command).await,
        RunMode::Interactive | RunMode::Notebook(_) => run_interactive(&mut shell, &mut ctx).await,
    }
}

/// Kills every managed `!` chat command when the shell leaves.
///
/// The registry behind it is a `LazyLock`, which is never dropped, so nothing
/// else runs `AgentJobs`' own `Drop` and the process groups would outlive the
/// shell. `std::process::exit` skips this, which is why the signal watcher in
/// `bootstrap.rs` calls the same function before exiting.
struct ChatJobsShutdown;

impl Drop for ChatJobsShutdown {
    fn drop(&mut self) {
        dsh_builtin::chat_jobs_shutdown();
    }
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

    // Initialize AI service for non-interactive commands.
    //
    // The service always exists and follows the shared `ai_client` slot (see
    // `Repl::new`): a key exported mid-script still takes effect, and callers
    // keep asking `ai_configured()` / the slot rather than `is_some()`.
    {
        use std::sync::Arc;

        let env_handle = Arc::clone(&shell.environment);
        let slot = env_handle.read().integration_state.ai_client.clone();
        let shared = crate::ai_features::SharedChatClient::new(slot);
        let mcp_manager = env_handle.read().integration_state.mcp_manager.clone();
        let safety_level = env_handle.read().policy_state.safety_level.clone();
        let policy = crate::ai_features::AgentPolicyHandles {
            safety_level,
            safety_guard: shell.safety_guard.clone(),
            execute_allowlist: env_handle.read().policy_state.execute_allowlist.clone(),
            agent_session_allowlist: env_handle
                .read()
                .policy_state
                .agent_session_allowlist
                .clone(),
        };
        let response_language = env_handle
            .read()
            .integration_state
            .response_language
            .clone();
        let chat_model = env_handle.read().integration_state.chat_model.clone();
        let service = Arc::new(crate::ai_features::LiveAiService::new(
            shared,
            mcp_manager,
            policy,
            None,
            response_language,
            chat_model,
        ));
        shell.environment.write().integration_state.ai_service = Some(service);
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

    // Herdr lifecycle reporting, if this process is running inside a Herdr
    // pane and `DOGESH_HERDR_ENABLED` is set (default off). Scoped to this
    // function (not `Repl::new`/`Drop for Repl`, which runs across unrelated
    // unit tests) and to interactive/notebook mode only - a one-shot `-c`/`-l`
    // invocation isn't the "pane a human is watching" model Herdr targets.
    // `_lifecycle_shutdown` releases authority on every return path out of
    // this function.
    let (lifecycle, owner_marker) = {
        let env = shell.environment.read();
        agent_lifecycle::activate(&env)
    };
    shell.environment.write().integration_state.lifecycle = lifecycle.clone();
    if let Some((key, value)) = owner_marker {
        // Published through the shell's own environment snapshot, not
        // `std::env::set_var`: `dsh/src/process/process.rs` builds each
        // spawned child's `envp` explicitly from
        // `Environment.variable_state.system_env_vars`, so a nested `dsh`
        // (or any other agent CLI started from inside this shell) only sees
        // this marker if it goes through that path.
        shell
            .environment
            .write()
            .set_system_env_var(key.to_string(), value);
        spawn_herdr_shutdown_signal_watcher(lifecycle.clone());
    }
    let _lifecycle_shutdown = agent_lifecycle::ShutdownGuard::new(lifecycle);

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
