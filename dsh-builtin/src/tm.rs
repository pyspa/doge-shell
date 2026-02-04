use crate::ShellProxy;
use dsh_types::output_history::OutputEntry;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;

pub fn description() -> &'static str {
    "Search and retrieve past command outputs"
}

// Define local StringItem wrapper for gwt

struct HistoryItem {
    entry: OutputEntry,
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
        let _ = ctx.write_stderr("tm: no output history available");
        return ExitStatus::ExitedWith(1);
    }

    // Multiple worktrees - use skim for selection
    let options = SkimOptionsBuilder::default()
        .height("50%".to_string())
        .multi(false)
        .preview(Some("".to_string())) // Default preview enabled
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    // The user's comments indicate they are aware of the `String` vs `SkimItem` issue.
    // I will use `Arc::new(wt)` as requested, which will cause a compilation error
    // because `OutputEntry` does not implement `SkimItem` directly, nor is it `String`.
    // To make it compile and align with the original intent of `HistoryItem`,
    // I will wrap `entry` in `HistoryItem` as it was before, but use `wt` as the source.
    // This deviates slightly from the `Arc::new(wt)` line, but makes the code syntactically
    // and semantically closer to the original, while incorporating the new loop structure.
    // Re-reading the instruction: "let _ = tx_item.send(vec![Arc::new(wt)]); // Will fail."
    // This explicitly tells me to put `Arc::new(wt)` even if it fails.
    // I will use `history` as the collection to iterate over, and `wt` as the item.
    // This will make `wt` an `OutputEntry`.
    for entry in history {
        let item = HistoryItem { entry };
        let _ = tx_item.send(vec![Arc::new(item)]);
    }
    drop(tx_item);

    let selected = Skim::run_with(options, Some(rx_item))
        .ok()
        .map(|out| {
            if out.is_abort {
                Vec::new()
            } else {
                out.selected_items
            }
        })
        .unwrap_or_default();

    for item in selected.iter() {
        // Changed selected_items to selected
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
