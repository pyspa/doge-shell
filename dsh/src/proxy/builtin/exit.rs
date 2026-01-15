//! Exit command handler.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;

/// Execute the `exit` builtin command.
///
/// Terminates the shell session.
pub fn execute(shell: &mut Shell, _ctx: &Context, _argv: Vec<String>) -> Result<()> {
    shell.exit();
    Ok(())
}
