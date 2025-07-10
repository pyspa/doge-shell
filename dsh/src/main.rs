use crate::environment::Environment;
use crate::repl::Repl;
use crate::shell::Shell;
use anyhow::Result;
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
    if let Err(err) = init_tracing() {
        eprintln!("Failed to initialize tracing: {err}");
        return ExitCode::FAILURE;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run_shell())
}

async fn run_shell() -> ExitCode {
    let cli = Cli::parse();
    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = create_context(&shell);

    if let Some(command) = cli.command.as_deref() {
        execute_command(&mut shell, &mut ctx, command).await
    } else {
        run_interactive(&mut shell, &mut ctx).await
    }
}

fn init_tracing() -> Result<()> {
    let log_file = std::sync::Arc::new(std::fs::File::create("./debug.log")?);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
    // tracing_subscriber::fmt::init();
    Ok(())
}

fn create_context(shell: &Shell) -> Context {
    let shell_tmode = tcgetattr(0).expect("failed tcgetattr");
    Context::new(shell.pid, shell.pgid, shell_tmode, true)
}

async fn execute_command(shell: &mut Shell, ctx: &mut Context, command: &str) -> ExitCode {
    debug!("start shell");
    shell.set_signals();

    match shell.eval_str(ctx, command.to_string(), false).await {
        Ok(code) => {
            debug!("run command mode {:?} : {:?}", command, &code);
            code
        }
        Err(err) => {
            eprintln!("{err:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run_interactive(shell: &mut Shell, ctx: &mut Context) -> ExitCode {
    debug!("start shell");
    shell.set_signals();
    ctx.save_history = false;

    // if let Err(err) = shell.eval_str(ctx, "cd .".to_string(), true) {
    //     eprintln!("{err:?}");
    //     return ExitCode::FAILURE;
    // }

    let mut repl = Repl::new(shell);
    if let Err(err) = repl.shell.eval_str(ctx, "cd .".to_string(), true).await {
        eprintln!("{err:?}");
        return ExitCode::FAILURE;
    }
    match repl.run_interactive().await {
        Err(err) => {
            eprintln!("{err:?}");
            ExitCode::FAILURE
        }
        _ => ExitCode::from(0),
    }
    // ExitCode::from(0)
}
