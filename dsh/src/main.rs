use crate::environment::Environment;
use crate::repl::Repl;
use crate::shell::Shell;
use async_std::task;
use clap::Parser;
use dsh_types::Context;
use nix::sys::termios::tcgetattr;
use std::process::ExitCode;
use tracing::debug;

mod completion;
mod direnv;
mod dirs;
mod environment;
mod history;
mod input;
mod lisp;
mod parser;
mod process;
mod prompt;
mod proxy;
mod repl;
mod shell;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    command: Option<String>,
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = create_context(&shell);

    if let Some(command) = cli.command.as_deref() {
        execute_command(&mut shell, &mut ctx, command)
    } else {
        run_interactive(&mut shell, &mut ctx)
    }
}

fn init_tracing() {
    tracing_subscriber::fmt::init();
}

fn create_context(shell: &Shell) -> Context {
    let shell_tmode = tcgetattr(0).expect("failed tcgetattr");
    Context::new(shell.pid, shell.pgid, shell_tmode, true)
}

fn execute_command(shell: &mut Shell, ctx: &mut Context, command: &str) -> ExitCode {
    debug!("start shell");
    shell.set_signals();

    match shell.eval_str(ctx, command.to_string(), false) {
        Ok(code) => {
            debug!("run command mode {:?} : {:?}", command, &code);
            code
        }
        Err(err) => {
            eprintln!("{:?}", err);
            ExitCode::FAILURE
        }
    }
}

fn run_interactive(shell: &mut Shell, ctx: &mut Context) -> ExitCode {
    debug!("start shell");
    shell.set_signals();
    shell.install_chpwd_hooks();
    ctx.save_history = false;

    if let Err(err) = shell.eval_str(ctx, "cd .".to_string(), true) {
        eprintln!("{:?}", err);
        return ExitCode::FAILURE;
    }

    let mut repl = Repl::new(shell);
    task::block_on(repl.run_interactive());
    ExitCode::from(0)
}
