use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;

pub(crate) const NAME: &str = "Search History";

pub struct SearchHistoryAction;

#[async_trait(?Send)]
impl Action for SearchHistoryAction {
    fn name(&self) -> &str {
        NAME
    }
    fn description(&self) -> &str {
        "Search command history"
    }
    fn icon(&self) -> &str {
        "📜"
    }

    async fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // CommandPalette::run hands control back to the REPL's history picker.
        Ok(())
    }
}
