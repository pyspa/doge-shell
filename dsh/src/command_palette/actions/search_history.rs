use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use skim::prelude::*;

pub struct SearchHistoryAction;

#[async_trait(?Send)]
impl Action for SearchHistoryAction {
    fn name(&self) -> &str {
        "Search History"
    }
    fn description(&self) -> &str {
        "Search command history"
    }
    fn icon(&self) -> &str {
        "📜"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        // Get command history
        let history_entries: Vec<String> = if let Some(ref history) = shell.cmd_history {
            let locked = history.lock();
            locked
                .iter()
                .rev()
                .take(1000)
                .map(|e| e.entry.clone())
                .collect()
        } else {
            return Err(anyhow::anyhow!("Command history not available"));
        };

        if history_entries.is_empty() {
            println!("No history entries");
            return Ok(());
        }

        use crate::command_palette::StringItem;

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("History> ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for entry in history_entries {
            let _ = tx.send(vec![Arc::new(StringItem(entry))]);
        }
        drop(tx);

        let selected = crate::utils::skim::run_skim_with(options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let command = item.output().to_string();
            // Print the selected command so user can see what was selected
            println!("{}", command);
        }

        Ok(())
    }
}
