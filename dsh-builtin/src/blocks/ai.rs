//! AI-assisted operations on one block: deterministic quick fixes first, falling back to an AI-suggested fix (`fix_block`/`fix_block_ai`), and asking the model to explain what a block's command did
//! (`explain_block_async`).
use super::*;
use crate::capability::AiCapability;
use dsh_openai::turn::truncate_middle;
use dsh_types::quick_fix::{DeterministicQuickFixProvider, QuickFix, QuickFixProvider};

fn deterministic_fixes(block: &CommandBlock) -> Vec<QuickFix> {
    let output = if block.stderr.is_empty() {
        block.stdout.as_str()
    } else {
        block.stderr.as_str()
    };
    DeterministicQuickFixProvider.suggest(&block.command, block.exit_code, output)
}
pub(super) fn fix_block(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
    json_output: bool,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };
    let fixes = deterministic_fixes(&block);
    if json_output {
        let _ = ctx.write_stdout(
            &serde_json::to_string(&json!({
                "block": index,
                "command": block.command,
                "fixes": fixes,
                "source": "deterministic"
            }))
            .unwrap_or_else(|_| "{}".to_string()),
        );
    } else if fixes.is_empty() {
        let _ = ctx.write_stdout("No deterministic fix found. Retry with `blocks fix <N> --ai`.");
    } else {
        let lines = fixes
            .iter()
            .enumerate()
            .map(|(offset, fix)| format!("{}. {}\n   {}", offset + 1, fix.title, fix.replacement))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = ctx.write_stdout(&lines);
    }
    ExitStatus::ExitedWith(0)
}
pub(super) async fn fix_block_ai(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
    json_output: bool,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };

    let deterministic = deterministic_fixes(&block);
    if !deterministic.is_empty() {
        return fix_block(ctx, proxy, index, json_output);
    }

    let output = if block.stderr.is_empty() {
        block.stdout.as_str()
    } else {
        block.stderr.as_str()
    };
    let messages = vec![
        json!({
            "role": "system",
            "content": "Return one corrected shell command only, with no prose and no translation of the command itself. Never claim it has been executed."
        }),
        json!({
            "role": "user",
            "content": format!("Command: {}\nExit code: {}\nOutput:\n{}", block.command, block.exit_code, truncate_middle(output, 4000))
        }),
    ];
    match proxy.ask(messages).await {
        Ok(replacement) => {
            if json_output {
                let _ = ctx.write_stdout(
                    &serde_json::to_string(&json!({
                        "block": index,
                        "command": block.command,
                        "fixes": [{
                            "id": "ai",
                            "title": "AI suggestion",
                            "replacement": replacement.trim()
                        }],
                        "source": "ai"
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                );
            } else {
                let _ = ctx.write_stdout(replacement.trim());
            }
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: AI fix failed: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}
pub(super) async fn explain_block_async(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };

    let output = if block.stdout.is_empty() {
        block.stderr.as_str()
    } else {
        block.stdout.as_str()
    };
    let output = truncate_middle(output, 4000);

    let messages = vec![
        json!({
            "role": "system",
            "content": "You are a shell command analyst. Explain this command block concisely, focusing on result, errors, and the next useful action."
        }),
        json!({
            "role": "user",
            "content": format!(
                "Command: `{}`\nExit code: {}\nDuration: {} ms\nOutput:\n```\n{}\n```",
                block.command, block.exit_code, block.duration_ms, output
            )
        }),
    ];

    match proxy.ask(messages).await {
        Ok(response) => {
            let _ = ctx.write_stdout(&response);
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: AI explanation failed: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}
