use crate::environment::Environment;
use crate::shell::Shell;
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
mod script;
mod shell;
mod wasm;

#[async_std::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();

    debug!("start shell");
    let env: Environment = Default::default();
    enable_raw_mode()?;

    let mut shell = Shell::new(env);
    async_std::task::block_on(shell.run_interactive());

    disable_raw_mode()
}
