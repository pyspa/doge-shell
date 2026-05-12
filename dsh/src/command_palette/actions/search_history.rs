use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use skim::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;

pub struct SearchHistoryAction;

struct HistoryItem {
    command: String,
    display: String,
}

impl SkimItem for HistoryItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.command)
    }
}

pub async fn select_history_command(shell: &mut Shell) -> Result<Option<String>> {
    let history_entries: Vec<HistoryItem> = if let Some(ref history) = shell.cmd_history {
        let locked = history.lock();
        locked
            .iter()
            .rev()
            .take(1000)
            .map(|entry| HistoryItem {
                command: entry.entry.clone(),
                display: format_history_item(entry),
            })
            .collect()
    } else {
        return Err(anyhow::anyhow!("Command history not available"));
    };

    if history_entries.is_empty() {
        return Ok(None);
    }

    let options = SkimOptionsBuilder::default()
        .prompt("History> ".to_string())
        .bind(vec!["Enter:accept".to_string(), "Esc:abort".to_string()])
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

    let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
    for entry in history_entries {
        let _ = tx.send(vec![Arc::new(entry)]);
    }
    drop(tx);

    let selected = crate::utils::skim::run_skim_with(options, Some(rx))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    Ok(selected.first().map(|item| item.output().to_string()))
}

fn format_history_item(entry: &crate::history::Entry) -> String {
    let status = match entry.exit_code {
        Some(0) => "ok".to_string(),
        Some(code) => format!("err:{code}"),
        None => "-".to_string(),
    };
    let duration = entry
        .duration_ms
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "-".to_string());
    let cwd = entry.cwd.as_deref().unwrap_or("-");
    format!("{status:>6} {duration:>8} {cwd}  {}", entry.entry)
}

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
        if let Some(command) = select_history_command(shell).await? {
            println!("{}", command);
        }
        Ok(())
    }
}
