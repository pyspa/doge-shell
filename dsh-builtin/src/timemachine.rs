use crate::ShellProxy;
use dsh_types::output_history::OutputEntry;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;

pub fn description() -> &'static str {
    "Search and retrieve past command outputs"
}

struct HistoryItem {
    entry: OutputEntry,
    index: usize,
}

impl SkimItem for HistoryItem {
    fn text(&self) -> Cow<'_, str> {
        let dt: chrono::DateTime<chrono::Local> = self.entry.timestamp.into();
        Cow::Owned(format!(
            "[{}] {} (Exit: {})",
            dt.format("%H:%M:%S"),
            self.entry.command,
            self.entry.exit_code
        ))
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        let mut content = String::new();
        if !self.entry.stdout.is_empty() {
            content.push_str("--- STDOUT ---\n");
            content.push_str(&self.entry.stdout);
            content.push('\n');
        }
        if !self.entry.stderr.is_empty() {
            content.push_str("--- STDERR ---\n");
            content.push_str(&self.entry.stderr);
            content.push('\n');
        }
        if content.is_empty() {
            content.push_str("(No Output)");
        }
        ItemPreview::Text(content)
    }
}

pub fn command(ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let history = proxy.get_full_output_history();

    if history.is_empty() {
        let _ = ctx.write_stderr("timemachine: no output history available");
        return ExitStatus::ExitedWith(1);
    }

    let options = SkimOptionsBuilder::default()
        .height("50%".to_string())
        .multi(false)
        .preview(Some("".to_string())) // Default preview enabled
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();

    for (i, entry) in history.into_iter().enumerate() {
        let item = HistoryItem {
            index: i + 1,
            entry,
        };
        let _ = tx_item.send(Arc::new(item));
    }
    drop(tx_item); // Close sender

    let selected_items = Skim::run_with(&options, Some(rx_item))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    for item in selected_items.iter() {
        if let Some(history_item) = (**item).as_any().downcast_ref::<HistoryItem>() {
            // Print the output to stdout so user can pipe it or view it
            if !history_item.entry.stdout.is_empty() {
                let _ = ctx.write_stdout(&history_item.entry.stdout);
            }
            if !history_item.entry.stderr.is_empty() {
                // If we write to stderr, it might not be captured if user pipes stdout.
                // But usually "retrieving output" implies re-emitting it.
                // Or maybe we should print separation?
                // The requirement is "retrieve past output".
                // Printing stdout to stdout and stderr to stderr seems correct.
                let _ = ctx.write_stderr(&history_item.entry.stderr);
            }
        }
    }

    ExitStatus::ExitedWith(0)
}
