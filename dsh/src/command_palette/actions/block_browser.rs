use super::super::Action;
use crate::blocks_ui::{BlockBrowser, BrowserOutcome, run};
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;

/// Open the browser and return the command the user picked, if any.
///
/// The palette hands this back to the caller, which puts it in the input buffer
/// — the same path `Search History` uses. Queueing it with
/// `Shell::request_eval_command` would not work here: the only drain runs inside
/// `handle_execute`, and the palette is dispatched from a different arm of the
/// REPL loop, so the command would sit unconsumed and then fire unexpectedly
/// after whatever the user typed next.
///
/// `Run` is therefore downgraded to "fill the buffer": the palette has no way to
/// execute, and silently deferring the execution is worse than making the user
/// press Enter.
pub async fn select_block_command(shell: &mut Shell) -> Result<Option<String>> {
    let blocks = shell.environment.read().command_blocks.get_all_blocks();
    if blocks.is_empty() {
        println!("No command blocks recorded yet.");
        return Ok(None);
    }

    Ok(match run(BlockBrowser::new(blocks))? {
        BrowserOutcome::Insert(text) | BrowserOutcome::Run(text) => Some(text),
        BrowserOutcome::Quit => None,
    })
}

pub struct BlockBrowserAction;

#[async_trait(?Send)]
impl Action for BlockBrowserAction {
    fn name(&self) -> &str {
        "Block Browser"
    }
    fn description(&self) -> &str {
        "Browse this session's commands with their captured output (Ctrl-O)"
    }
    fn icon(&self) -> &str {
        "🧱"
    }

    /// Normally unreachable: `CommandPalette::run` intercepts this action by
    /// name so it can return the picked command, which the `Action` trait
    /// cannot express. Kept working — minus the hand-back — in case it is not.
    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        select_block_command(shell).await?;
        Ok(())
    }
}
