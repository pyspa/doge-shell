//! What the user just ran, and what happened.
//!
//! The shell records every foreground command as a `CommandBlock` - the text,
//! the directory, the exit code, both output streams - and the agent could not
//! see any of it. "Fix the error I just got" therefore meant guessing at the
//! command and running it again, which is slower, costs a turn, and is wrong
//! outright when the failure is not reproducible.

use crate::ShellProxy;
use crate::safety_policy;
use dsh_types::command_block::CommandBlock;
use serde_json::{Value, json};

pub(crate) const NAME: &str = "shell_history";

/// Blocks listed when the caller does not ask for a number.
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;
/// Output shown per block in the list view.
const SUMMARY_OUTPUT_CHARS: usize = 240;
/// Output shown when one block is asked for by id.
const DETAIL_OUTPUT_CHARS: usize = 4096;

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "List commands the user recently ran in this shell, with their working directory, exit code and a short piece of their output. Use this first when the user refers to something that already happened (\"the error just now\", \"why did that fail\") instead of running the command again. Pass `id` to get the full output of one entry.",
            "parameters": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "How many entries to list, newest first. Defaults to 5."
                    },
                    "only_failed": {
                        "type": "boolean",
                        "description": "List only commands that exited non-zero. Defaults to true."
                    },
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Show one entry in full instead of listing. Ids come from the list."
                    }
                },
                "required": [],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(arguments)
            .map_err(|err| format!("chat: invalid JSON arguments for {NAME} tool: {err}"))?
    };

    let blocks = proxy.get_command_blocks();
    if blocks.is_empty() {
        return Ok("No commands have been recorded in this session yet.".to_string());
    }

    if let Some(id) = parsed.get("id").and_then(Value::as_u64) {
        return Ok(match blocks.iter().find(|block| block.id == id) {
            Some(block) => render_detail(block),
            None => format!("No recorded command has id {id}."),
        });
    }

    let limit = parsed
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| (limit.max(1) as usize).min(MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);
    let only_failed = parsed
        .get("only_failed")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    render_list(&blocks, limit, only_failed)
}

/// `get_command_blocks` yields newest first.
fn render_list(blocks: &[CommandBlock], limit: usize, only_failed: bool) -> Result<String, String> {
    let selected: Vec<&CommandBlock> = blocks
        .iter()
        .filter(|block| !only_failed || block.exit_code != 0)
        .take(limit)
        .collect();

    if selected.is_empty() {
        // Saying "nothing" without saying why sends the agent looking for a
        // failure that is simply not in this view.
        return Ok(if only_failed {
            "No failed commands recorded in this session. Call again with only_failed=false to see successful ones.".to_string()
        } else {
            "No commands recorded in this session.".to_string()
        });
    }

    let mut out = String::from("Recent commands, newest first:\n");
    for block in selected {
        out.push_str(&render_summary(block));
    }
    out.push_str("\nCall shell_history with `id` to read one entry's full output.");
    Ok(out)
}

fn render_summary(block: &CommandBlock) -> String {
    let mut line = format!(
        "\n[{}] exit={} {}ms  {}\n",
        block.id,
        block.exit_code,
        block.duration_ms,
        redact(&block.command)
    );

    if let Some(cwd) = &block.cwd {
        line.push_str(&format!("    cwd: {cwd}\n"));
    }

    let output = combined_output(block, SUMMARY_OUTPUT_CHARS);
    if !output.trim().is_empty() {
        for output_line in output.lines() {
            line.push_str(&format!("    | {output_line}\n"));
        }
    }
    line
}

fn render_detail(block: &CommandBlock) -> String {
    let mut out = format!(
        "[{}] exit={} {}ms  {}\n",
        block.id,
        block.exit_code,
        block.duration_ms,
        redact(&block.command)
    );
    if let Some(cwd) = &block.cwd {
        out.push_str(&format!("cwd: {cwd}\n"));
    }

    let stdout = redact(&block.stdout);
    let stderr = redact(&block.stderr);
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        out.push_str("(no output)");
        return out;
    }

    // Half the budget each, so a chatty stdout cannot push out the stderr that
    // usually carries the error.
    let half = DETAIL_OUTPUT_CHARS / 2;
    if !stdout.trim().is_empty() {
        out.push_str("stdout:\n");
        out.push_str(&dsh_openai::turn::truncate_middle(&stdout, half));
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("stderr:\n");
        out.push_str(&dsh_openai::turn::truncate_middle(&stderr, half));
    }
    out
}

fn combined_output(block: &CommandBlock, budget: usize) -> String {
    // stderr first: a failure explains itself there far more often than in
    // stdout, and this preview is short.
    let source = if block.stderr.trim().is_empty() {
        &block.stdout
    } else {
        &block.stderr
    };
    dsh_openai::turn::truncate_middle(&redact(source), budget)
}

fn redact(text: &str) -> String {
    safety_policy::redact_sensitive_text(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn block(id: u64, command: &str, exit_code: i32, stdout: &str, stderr: &str) -> CommandBlock {
        CommandBlock {
            id,
            command: command.to_string(),
            cwd: Some("/work".to_string()),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            timestamp: SystemTime::UNIX_EPOCH,
            duration_ms: 12,
            output_entry_ids: Vec::new(),
            watched: false,
            watch_summary: None,
        }
    }

    #[test]
    fn list_defaults_to_failures_only() {
        let blocks = vec![
            block(3, "ls", 0, "a\nb", ""),
            block(2, "cargo build", 101, "", "error[E0308]: mismatched types"),
        ];

        let rendered = render_list(&blocks, 5, true).unwrap();

        assert!(rendered.contains("cargo build"), "{rendered}");
        assert!(!rendered.contains("[3]"), "{rendered}");
        assert!(rendered.contains("E0308"), "{rendered}");
    }

    /// An empty view has to say which filter produced it.
    #[test]
    fn an_empty_failure_list_points_at_the_filter() {
        let blocks = vec![block(1, "ls", 0, "a", "")];

        let rendered = render_list(&blocks, 5, true).unwrap();

        assert!(rendered.contains("only_failed=false"), "{rendered}");
    }

    /// The preview prefers stderr, which is where the error usually is.
    #[test]
    fn the_summary_prefers_stderr() {
        let block = block(1, "make", 2, "compiling...", "No rule to make target");

        let rendered = render_summary(&block);

        assert!(rendered.contains("No rule to make target"), "{rendered}");
        assert!(!rendered.contains("compiling..."), "{rendered}");
    }

    /// Both streams reach the model when one entry is asked for by id.
    #[test]
    fn the_detail_view_keeps_both_streams() {
        let block = block(1, "make", 2, "compiling...", "No rule to make target");

        let rendered = render_detail(&block);

        assert!(rendered.contains("compiling..."), "{rendered}");
        assert!(rendered.contains("No rule to make target"), "{rendered}");
    }

    /// Command output regularly carries a token; it must not leave the shell.
    #[test]
    fn secrets_are_masked_before_the_model_sees_them() {
        let block = block(1, "deploy", 1, "", "AWS_SECRET_ACCESS_KEY=abcd1234efgh");

        let rendered = render_detail(&block);

        assert!(!rendered.contains("abcd1234efgh"), "{rendered}");
    }
}
