//! History command handler.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;

/// Execute the `history` builtin command.
///
/// Displays the command history.
pub fn execute(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    if let Some(ref mut history) = shell.cmd_history {
        let mut history = history.lock();
        for item in history.iter() {
            ctx.write_stdout(&item.entry)?;
        }
        history.reset_index();
    }
    Ok(())
}
