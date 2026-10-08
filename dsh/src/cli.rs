//! Command-line argument parsing: the `clap` definitions and the run mode
//! they resolve to. No behavior lives here beyond `RunMode::from_cli`.
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None, args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// Command followed by its optional invocation name and arguments.
    /// Consume option-looking values here as well as in the trailing positional.
    #[arg(short, long, num_args = 1.., allow_hyphen_values = true)]
    pub command: Option<Vec<String>>,

    /// COMMAND_NAME and arguments for -c (native script mode is not supported).
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        requires = "command"
    )]
    pub command_args: Vec<String>,

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

    /// Execute a Lisp script file as a program
    Lisp {
        /// Lisp script file, resolved against the current working directory
        file: PathBuf,

        /// Arguments passed to the script (`*argv*`); option-looking
        /// values are accepted, use `--` to separate them from the file
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunMode {
    Interactive,
    Command {
        command: String,
        argv0: String,
        positional: Vec<String>,
    },
    LispInline(String),
    LispFile {
        path: PathBuf,
        argv0: String,
        positional: Vec<String>,
    },
    Notebook(PathBuf),
}

impl RunMode {
    pub(crate) fn from_cli(cli: &Cli) -> Self {
        if let Some(SubCommand::Lisp { file, args }) = &cli.subcommand {
            Self::LispFile {
                path: file.clone(),
                argv0: file.to_string_lossy().into_owned(),
                positional: args.clone(),
            }
        } else if let Some(script) = &cli.lisp {
            Self::LispInline(script.clone())
        } else if let Some(command) = &cli.command {
            Self::Command {
                command: command[0].clone(),
                argv0: command
                    .iter()
                    .skip(1)
                    .chain(cli.command_args.iter())
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "dogesh".into()),
                positional: command
                    .iter()
                    .skip(1)
                    .chain(cli.command_args.iter())
                    .skip(1)
                    .cloned()
                    .collect(),
            }
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
            command_args: Vec::new(),
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
            command: Some(vec!["true".to_string()]),
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
    fn lisp_file_subcommand_parses_path_and_arguments() {
        let cli = Cli::try_parse_from(["dogesh", "lisp", "script.lisp"]).unwrap();
        assert_eq!(
            RunMode::from_cli(&cli),
            RunMode::LispFile {
                path: PathBuf::from("script.lisp"),
                argv0: "script.lisp".into(),
                positional: vec![],
            }
        );
        assert!(!RunMode::from_cli(&cli).needs_interactive_services());

        let cli = Cli::try_parse_from(["dogesh", "lisp", "script.lisp", "alpha", "beta"]).unwrap();
        assert_eq!(
            RunMode::from_cli(&cli),
            RunMode::LispFile {
                path: PathBuf::from("script.lisp"),
                argv0: "script.lisp".into(),
                positional: vec!["alpha".into(), "beta".into()],
            }
        );

        let cli =
            Cli::try_parse_from(["dogesh", "lisp", "script.lisp", "--", "--flag", "-x"]).unwrap();
        assert_eq!(
            RunMode::from_cli(&cli),
            RunMode::LispFile {
                path: PathBuf::from("script.lisp"),
                argv0: "script.lisp".into(),
                positional: vec!["--flag".into(), "-x".into()],
            }
        );

        let cli = Cli::try_parse_from(["dogesh", "lisp", "./dir/script.lisp"]).unwrap();
        assert_eq!(
            RunMode::from_cli(&cli),
            RunMode::LispFile {
                path: PathBuf::from("./dir/script.lisp"),
                argv0: "./dir/script.lisp".into(),
                positional: vec![],
            }
        );

        assert!(Cli::try_parse_from(["dogesh", "lisp"]).is_err());
    }
    #[test]
    fn invocation_cli_preserves_subcommands_and_trailing_arguments() {
        let cli = Cli::try_parse_from(["dogesh", "completion", "git"]).unwrap();
        assert!(
            matches!(cli.subcommand, Some(SubCommand::Completion { command, .. }) if command == "git")
        );
        let cli = Cli::try_parse_from(["dogesh", "import", "fish"]).unwrap();
        assert!(
            matches!(cli.subcommand, Some(SubCommand::Import { shell, .. }) if shell == "fish")
        );
        for arg in ["value", "-x", "--foo"] {
            let cli = Cli::try_parse_from(["dogesh", "-c", "echo \"$1\"", "name", arg]).unwrap();
            assert_eq!(cli.command.as_ref().unwrap(), &["echo \"$1\"", "name", arg]);
            assert_eq!(
                RunMode::from_cli(&cli),
                RunMode::Command {
                    command: "echo \"$1\"".into(),
                    argv0: "name".into(),
                    positional: vec![arg.into()]
                }
            );
        }
        assert!(Cli::try_parse_from(["dogesh", "foo"]).is_err());
    }
    #[test]
    fn command_name_can_match_a_subcommand() {
        for name in ["import", "completion", "lisp"] {
            let cli = Cli::try_parse_from(["dogesh", "-c", "echo", name, "arg"]).unwrap();
            assert!(cli.subcommand.is_none());
            assert_eq!(cli.command.as_ref().unwrap(), &["echo", name, "arg"]);
        }
    }
    #[test]
    fn option_looking_command_and_invocation_names_are_values() {
        for name in ["--help", "-l", "-c", "--notebook", "--", ""] {
            let cli = Cli::try_parse_from(["dogesh", "-c", "echo", name, "arg"]).unwrap();
            assert_eq!(
                RunMode::from_cli(&cli),
                RunMode::Command {
                    command: "echo".into(),
                    argv0: name.into(),
                    positional: vec!["arg".into()],
                }
            );
        }
        let cli = Cli::try_parse_from(["dogesh", "-c", "-command"]).unwrap();
        assert_eq!(
            RunMode::from_cli(&cli),
            RunMode::Command {
                command: "-command".into(),
                argv0: "dogesh".into(),
                positional: vec![],
            }
        );
    }
}
