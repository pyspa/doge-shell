use super::super::Action;
use crate::blocks_ui::{BlockBrowser, BrowserOutcome, run};
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;

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

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let blocks = shell.environment.read().command_blocks.get_all_blocks();
        if blocks.is_empty() {
            println!("No command blocks recorded yet.");
            return Ok(());
        }

        match run(BlockBrowser::new(blocks))? {
            // The palette runs outside the line editor, so there is no input
            // buffer to fill: queue the command for the shell instead.
            BrowserOutcome::Insert(text) | BrowserOutcome::Run(text) => {
                shell.request_eval_command(text)?;
            }
            BrowserOutcome::Quit => {}
        }
        Ok(())
    }
}
