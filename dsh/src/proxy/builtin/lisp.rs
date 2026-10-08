//! Lisp command handlers.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use tracing::debug;

/// Execute the `lisp` builtin command.
///
/// Evaluates a Lisp expression directly. Evaluation failures propagate as
/// `Err` so the builtin reports a non-zero status; diagnostics belong to
/// the outer wrapper (`dsh-builtin`), not this proxy layer.
pub fn execute_lisp(shell: &mut Shell, _ctx: &Context, argv: Vec<String>) -> Result<()> {
    let value = shell.lisp_engine.borrow().run(argv[1].as_str())?;
    debug!("{}", value);
    Ok(())
}

/// Execute the `lisp-run` builtin command.
///
/// Runs a Lisp function with arguments. Legacy path: kept unregistered and
/// unchanged in behavior except that evaluation failures now propagate.
pub fn execute_lisp_run(shell: &mut Shell, _ctx: &Context, argv: Vec<String>) -> Result<()> {
    let mut argv = argv;
    let cmd = argv.remove(0);
    let value = shell.lisp_engine.borrow().run_func(cmd.as_str(), argv)?;
    debug!("{}", value);
    Ok(())
}
