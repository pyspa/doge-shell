//! Exporting command blocks as a runbook: selecting which blocks (`select_blocks_for_export`), writing the markdown file (`write_runbook`), and the AI-titled variant that asks the model for a
//! one-line description of each step (`export_blocks_ai`).
use super::*;
use crate::capability::AiCapability;
use dsh_openai::turn::truncate_middle;

/// Resolve an export selection against the newest-first block list and return
/// the chosen blocks in chronological order — a runbook is a procedure, so
/// steps must read in execution order.
pub(super) fn select_blocks_for_export(
    blocks: &[CommandBlock],
    selection: &ExportSelection,
) -> Result<Vec<CommandBlock>, String> {
    if blocks.is_empty() {
        return Err("no command blocks available".to_string());
    }

    let selected: Vec<CommandBlock> = match selection {
        ExportSelection::Range(start, end) => {
            if *end > blocks.len() {
                return Err(format!(
                    "range end {end} exceeds available blocks ({})",
                    blocks.len()
                ));
            }
            blocks[start - 1..*end].to_vec()
        }
        ExportSelection::Last(count) => blocks.iter().take(*count).cloned().collect(),
        ExportSelection::Ids(ids) => {
            let mut selected = Vec::with_capacity(ids.len());
            for id in ids {
                let Some(block) = blocks.iter().find(|block| block.id == *id) else {
                    return Err(format!("no block with id {id}"));
                };
                selected.push(block.clone());
            }
            // Ids may arrive in any order; sort newest-first like the other
            // arms so the single reversal below yields chronological order.
            selected.sort_by_key(|block| std::cmp::Reverse(block.id));
            selected
        }
    };

    Ok(selected.into_iter().rev().collect())
}
pub(super) fn export_blocks(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    selection: &ExportSelection,
    output: Option<&str>,
    title: Option<String>,
    descriptions: Option<Vec<String>>,
) -> ExitStatus {
    let blocks = proxy.get_command_blocks();
    let selected = match select_blocks_for_export(&blocks, selection) {
        Ok(selected) => selected,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    };

    write_runbook(ctx, &selected, output, title, descriptions)
}
/// Render the already-selected blocks and write them out.
///
/// Takes the resolved blocks rather than the selection so the AI path cannot
/// re-resolve after its await: a block recorded meanwhile would shift what
/// `--last`/`--range` mean and attach each description to the wrong step.
fn write_runbook(
    ctx: &Context,
    selected: &[CommandBlock],
    output: Option<&str>,
    title: Option<String>,
    descriptions: Option<Vec<String>>,
) -> ExitStatus {
    let options = crate::runbook::RunbookOptions {
        title,
        descriptions,
        ..Default::default()
    };
    let markdown = crate::runbook::render_runbook(selected, &options);

    match output {
        Some(path) => match std::fs::write(path, &markdown) {
            Ok(()) => {
                let _ = ctx.write_stdout(&format!(
                    "Exported {} block(s) to {path}. Replay with `notebook-play {path}`.",
                    selected.len()
                ));
                ExitStatus::ExitedWith(0)
            }
            Err(err) => {
                let _ = ctx.write_stderr(&format!("blocks: failed to write {path}: {err}"));
                ExitStatus::ExitedWith(1)
            }
        },
        None => {
            let _ = ctx.write_stdout(&markdown);
            ExitStatus::ExitedWith(0)
        }
    }
}
pub(super) async fn export_blocks_ai(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    selection: &ExportSelection,
    output: Option<&str>,
    title: Option<String>,
) -> ExitStatus {
    let blocks = proxy.get_command_blocks();
    let selected = match select_blocks_for_export(&blocks, selection) {
        Ok(selected) => selected,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    };

    // One request for the whole runbook; a failed or unparsable response
    // degrades to an export without descriptions instead of failing.
    let steps = selected
        .iter()
        .enumerate()
        .map(|(index, block)| {
            format!(
                "{}. `{}` (exit {})\n{}",
                index + 1,
                block.command,
                block.exit_code,
                truncate_middle(
                    if block.stdout.is_empty() {
                        &block.stderr
                    } else {
                        &block.stdout
                    },
                    1000
                )
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let messages = vec![
        json!({
            "role": "system",
            "content": "You annotate shell runbooks. For each numbered step, describe its purpose in one short sentence. Reply with one line per step in the form `N. description`, nothing else."
        }),
        json!({
            "role": "user",
            "content": steps
        }),
    ];

    let descriptions = match proxy.ask(messages).await {
        Ok(response) => {
            let parsed = parse_numbered_descriptions(&response, selected.len());
            if parsed.iter().all(|line| line.is_empty()) {
                let _ = ctx.write_stderr(
                    "blocks: could not parse AI descriptions; exporting without them",
                );
                None
            } else {
                Some(parsed)
            }
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!(
                "blocks: AI description failed ({err}); exporting without descriptions"
            ));
            None
        }
    };

    // The blocks resolved before the await, so the descriptions line up with
    // exactly the steps they were generated for.
    write_runbook(ctx, &selected, output, title, descriptions)
}
/// Parse `N. description` lines into a per-step vector; unmatched steps stay
/// empty and simply render bare.
pub(super) fn parse_numbered_descriptions(response: &str, steps: usize) -> Vec<String> {
    let mut descriptions = vec![String::new(); steps];
    for line in response.lines() {
        let trimmed = line.trim().trim_start_matches(['-', '*']).trim_start();
        let Some((number, rest)) = trimmed.split_once(['.', ')']) else {
            continue;
        };
        let Ok(step) = number.trim().parse::<usize>() else {
            continue;
        };
        if step == 0 || step > steps {
            continue;
        }
        let text = rest.trim();
        if !text.is_empty() {
            descriptions[step - 1] = text.to_string();
        }
    }
    descriptions
}
