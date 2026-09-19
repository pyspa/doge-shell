//! Command-line argument parsing: the `clap` definitions and the run mode
//! they resolve to. No behavior lives here beyond `RunMode::from_cli`.
use clap::Parser;
use std::path::PathBuf;

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

    /// Internal re-exec helper: read one versioned exec request from this fd
    /// and run it, bypassing all interactive startup. Product-internal
    /// protocol, never shown in `--help`.
    #[arg(long = "__dsh-internal-exec-fd", hide = true)]
    pub internal_exec_fd: Option<i32>,

    /// Internal re-exec helper: one-byte completion report goes to this fd.
    /// Absent for helpers without a status channel (background builtins).
    #[arg(long = "__dsh-internal-status-fd", hide = true)]
    pub internal_status_fd: Option<i32>,

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

        /// Output file path (default: ~/.config/dogesh/completions/<command>.json)
        #[arg(short, long)]
        output: Option<String>,

        /// Force overwrite existing completion file
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunMode {
    Interactive,
    Command(String),
    Lisp(String),
    Notebook(PathBuf),
}

impl RunMode {
    pub(crate) fn from_cli(cli: &Cli) -> Self {
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

    pub(crate) fn needs_interactive_services(&self) -> bool {
        matches!(self, Self::Interactive | Self::Notebook(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_mode_limits_interactive_services_to_interactive_and_notebook() {
        let base = Cli {
            command: None,
            lisp: None,
            notebook: None,
            internal_exec_fd: None,
            internal_status_fd: None,
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
}
