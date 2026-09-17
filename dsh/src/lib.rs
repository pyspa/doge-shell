//! doge-shell crate root: module map, startup entry point (`lib_main`), and
//! the normal-exit error type threaded through the whole eval path.
//!
//! Process bootstrap (tracing, panic handler, startup background tasks) is
//! in `bootstrap`; CLI parsing and run-mode selection in `cli`; the four
//! ways a process can run (interactive, `-c`, `-l`, notebook) plus `Context`
//! construction in `run_modes`; `import`/`completion` subcommand handlers in
//! `subcommands`.

// doge-shell targets exactly two platforms. `#[cfg(not(target_os = "macos"))]`
// arms across the tree are written to mean "Linux"; this is what makes that
// reading true instead of merely usual.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("doge-shell supports Linux and macOS only");

use std::process::ExitCode;

pub mod agent;
pub mod agent_lifecycle;
pub mod ai_features;
pub mod argument_explainer;
pub mod blocks_ui;
mod bootstrap;
mod cli;
pub mod command_palette;
pub mod command_suggestion;
pub mod command_timing;
pub mod completion;
pub mod cron;
pub mod db;
pub mod detached_child;
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
mod run_modes;
pub mod safety;
pub mod secrets;
pub mod shell;
pub mod snippet;
mod subcommands;
pub mod suggestion;
pub mod terminal;
pub mod utils;

pub use bootstrap::{init_tracing, setup_panic_handler};
pub use cli::{Cli, SubCommand};
pub use run_modes::{
    create_context, create_context_for_command, execute_command, execute_lisp, run_interactive,
    run_shell,
};
pub use subcommands::{handle_completion_command, handle_import_command};

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
