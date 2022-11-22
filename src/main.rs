use crate::environment::Environment;
use crate::shell::Shell;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use std::io::stdout;

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
    env_logger::init();

    let env = Environment::new();
    let mut stdout = stdout();
    execute!(stdout, EnableMouseCapture)?;
    enable_raw_mode()?;

    let mut shell = Shell::new(env);
    async_std::task::block_on(shell.run_interactive());

    execute!(stdout, DisableMouseCapture)?;

    disable_raw_mode()
}
