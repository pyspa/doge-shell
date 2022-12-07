use crate::environment::Environment;
use crate::repl::Repl;
use crate::shell::Shell;
use clap::Parser;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use tracing::debug;

mod builtin;
mod completion;
mod config;
mod dirs;
mod environment;
mod frecency;
mod history;
mod input;
mod parser;
mod process;
mod prompt;
mod repl;
mod script;
mod shell;
mod wasm;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    command: Option<String>,
}

#[async_std::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    debug!("start shell");
    let env: Environment = Default::default();
    let mut shell = Shell::new(env);

    if let Some(command) = cli.command.as_deref() {
        enable_raw_mode()?;
        let _ = shell.eval_str(command.to_string(), false);
        disable_raw_mode()
    } else {
        enable_raw_mode()?;
        let mut repl = Repl::new(shell);
        async_std::task::block_on(repl.run_interactive());
        disable_raw_mode()
    }
}
