use crate::environment::Environment;
use crate::repl::Repl;
use crate::shell::Shell;
use clap::Parser;
use std::process::ExitCode;
use tracing::debug;

mod builtin;
mod completion;
mod dirs;
mod environment;
mod frecency;
mod history;
mod input;
mod lisp;
mod parser;
mod process;
mod prompt;
mod repl;
mod shell;
mod wasm;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    command: Option<String>,
}

#[async_std::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    debug!("start shell");
    let env = Environment::new();
    let mut shell = Shell::new(env);
    shell.set_signals();

    if let Some(command) = cli.command.as_deref() {
        match shell.eval_str(command.to_string(), false) {
            Ok(code) => {
                tracing::debug!("run command mode {:?} : {:?}", command, &code);
                code
            }
            Err(err) => {
                eprintln!("{:?}", err);
                ExitCode::FAILURE
            }
        }
    } else {
        let mut repl = Repl::new(shell);
        async_std::task::block_on(repl.run_interactive());
        ExitCode::from(0)
    }
}
