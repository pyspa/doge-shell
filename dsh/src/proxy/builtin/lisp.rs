//! Lisp command handlers.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use tracing::debug;

/// Execute the `lisp` builtin command.
///
/// Evaluates a Lisp expression directly.
pub fn execute_lisp(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    match shell.lisp_engine.borrow().run(argv[1].as_str()) {
        Ok(val) => {
            debug!("{}", val);
        }
        Err(err) => {
            ctx.write_stderr(&format!("{err}"))?;
        }
    }
    Ok(())
}

/// Execute the `lisp-run` builtin command.
///
/// Runs a Lisp function with arguments.
pub fn execute_lisp_run(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    let mut argv = argv;
    let cmd = argv.remove(0);
    match shell.lisp_engine.borrow().run_func(cmd.as_str(), argv) {
        Ok(val) => {
            debug!("{}", val);
        }
        Err(err) => {
            ctx.write_stderr(&format!("{err}"))?;
        }
    }
    Ok(())
}
