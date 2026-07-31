//! Proxy-side handler for `blocks tui`.
//!
//! The browser lives in the `dsh` crate because it needs clipboard and terminal
//! code that `dsh-builtin` cannot depend on, so the builtin dispatches here.

use crate::blocks_ui::{BlockBrowser, BrowserOutcome, run};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;

pub fn execute(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    let blocks = shell
        .environment
        .read()
        .session_output_state
        .command_blocks
        .get_all_blocks();
    if blocks.is_empty() {
        ctx.write_stdout("No command blocks available.")?;
        return Ok(());
    }

    match run(BlockBrowser::new(blocks))? {
        // There is no input buffer to write back into — the builtin runs as a
        // command, not from the line editor. `r` asked for execution, so queue
        // it; this drains in `handle_execute` right after the builtin returns.
        BrowserOutcome::Run(text) => {
            shell.request_eval_command(text)?;
        }
        // Enter means "give me the command", not "run it". Print it rather than
        // silently promoting it to an execution the user did not ask for.
        BrowserOutcome::Insert(text) => {
            ctx.write_stdout(&text)?;
        }
        BrowserOutcome::Quit => {}
    }
    Ok(())
}
