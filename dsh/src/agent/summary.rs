//! Turns a finished [`AgentTask`] (and its recorded events) into a short,
//! human-readable report - what `cron logs` puts in an AI job's `stdout`, and
//! what `agent show --summary` prints instead of the full JSON dump.
//!
//! # This is not a replacement for `agent show`
//!
//! `agent show <id>` with no flag is unchanged and stays the ground truth:
//! `docs/ai/skills/dsh-cron/references/troubleshooting.md` tells a person to
//! copy an `--allow-mcp` approval key from it byte-for-byte, and that key
//! lives in `events`, not in anything this module produces. This is a
//! reading aid layered on top, not a substitute.
//!
//! # Where the final answer comes from
//!
//! `AgentTask.checkpoint` is `Option<Value>` - an opaque blob as far as the
//! type system is concerned. Its actual shape is `ConversationManager`'s own
//! serialisation (`dsh-builtin/src/chatgpt/conversation.rs`; the type itself
//! is `pub(super)` there, so there is no shared Rust type for it across the
//! crate boundary, only this JSON shape). Its `buffer` array is the
//! conversation so far, and the chat loop (`dsh-builtin/src/chatgpt.rs`)
//! appends the assistant's message before returning the turn's answer, so
//! the most recent non-empty assistant message in `buffer` *is* the answer,
//! when the task ever got far enough to give one. Because that shape is not
//! a type-checked contract, [`final_answer`] returns `None` on anything it
//! does not recognise rather than guessing - and [`task_summary`] says so
//! explicitly (`(no final answer recorded)`) rather than silently dropping
//! the section, so a shape change upstream in `dsh-builtin` becomes visible
//! here instead of just making every summary quietly worse.
//!
//! # The byte budget
//!
//! `runs.stdout` is clamped to 8 KiB by `clamp_stream`
//! (`dsh/src/cron/store/rows.rs`, `pub(super)` there and unreachable from
//! this module) by keeping the head and the tail and dropping the middle -
//! exactly wrong for a structured report, which would have its criteria list
//! sheared out of the center. [`SUMMARY_BUDGET_BYTES`] keeps this module's
//! own output comfortably under that limit, so this module is never the
//! thing `clamp_stream` has to cut.

use dsh_types::agent::{AgentTask, TaskEvent, TaskStatus};
use dsh_types::text::clamp_chars;
use serde_json::Value;

/// Comfortably under `clamp_stream`'s 8 KiB - see this module's own doc
/// comment for why that matters.
const SUMMARY_BUDGET_BYTES: usize = 6 * 1024;
const HEADLINE_CHARS: usize = 120;
const GOAL_CHARS: usize = 200;
const STOP_REASON_CHARS: usize = 300;
const CRITERION_CHARS: usize = 150;
const PROGRESS_CHARS: usize = 800;
const FAILURE_RESULT_CHARS: usize = 200;
const FINAL_ANSWER_CHARS: usize = 2000;

/// The final, byte-based safety net: whatever the fixed-size sections above
/// add up to, this module's own output must never be the thing that gets
/// sheared in half by `clamp_stream` on the way into the store.
fn clamp_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const MARKER: &str = "\n... [truncated] ...\n";
    let budget = max_bytes.saturating_sub(MARKER.len());
    let cut = text.floor_char_boundary(budget);
    format!("{}{MARKER}", &text[..cut])
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "running",
        TaskStatus::InputRequired => "input-required",
        TaskStatus::Interrupted => "interrupted",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

/// The most recent non-empty assistant message in a task's checkpoint - the
/// turn's own answer, if the task ever got far enough to give one. See this
/// module's own doc comment for why the shape is read this defensively.
pub(crate) fn final_answer(checkpoint: Option<&Value>) -> Option<String> {
    let buffer = checkpoint?.get("buffer")?.as_array()?;
    buffer.iter().rev().find_map(|message| {
        if message.get("role")?.as_str()? != "assistant" {
            return None;
        }
        dsh_openai::turn::extract_message_content(message)
    })
}

/// Counts and names from a task's `tool_intent`/`tool_result` events -
/// enough to say what a run mostly did and, if it failed, what it was doing
/// when it did. The full argument/result text stays in `agent show <id>`;
/// repeating it here would blow the byte budget for exactly the runs (many
/// tool calls) where it matters least to see each one in full.
struct ToolStats {
    calls: usize,
    failed: usize,
    by_name: Vec<(String, usize)>,
    last_failure: Option<(String, String)>,
}

fn tool_name(call: Option<&Value>) -> &str {
    call.and_then(|call| call.get("function"))
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("?")
}

fn tool_stats(events: &[TaskEvent]) -> ToolStats {
    let mut calls = 0;
    let mut failed = 0;
    let mut by_name: Vec<(String, usize)> = Vec::new();
    let mut last_failure = None;
    for event in events {
        if event.kind != "tool_result" {
            continue;
        }
        calls += 1;
        let name = tool_name(event.data.get("call")).to_string();
        match by_name.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => by_name.push((name.clone(), 1)),
        }
        if event
            .data
            .get("failed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            failed += 1;
            let result = event
                .data
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("");
            last_failure = Some((name, clamp_chars(result, FAILURE_RESULT_CHARS)));
        }
    }
    ToolStats {
        calls,
        failed,
        by_name,
        last_failure,
    }
}

/// A human-readable report of one finished (or interrupted) agent task: what
/// it was asked to do, whether its own completion criteria passed, what it
/// finally said, and - if it did not finish - what it was doing when it
/// stopped. This is what an unattended `cron` run's `stdout` holds, and what
/// `agent show --summary` prints.
///
/// `succeeded` here is an approximation of `TaskRunReport.succeeded`
/// (`dsh/src/agent.rs`), not a byte-for-byte copy of it: the report also
/// factors in the chat subprocess's own exit status, which is not part of
/// `AgentTask` and is not available to a caller (`cron logs`'s live
/// fallback, in particular) that only has the task record to go on.
pub(crate) fn task_summary(task: &AgentTask, events: &[TaskEvent]) -> String {
    let answer = final_answer(task.checkpoint.as_ref());
    let non_empty_answer = answer.as_deref().map(str::trim).filter(|a| !a.is_empty());
    let succeeded =
        task.status == TaskStatus::Completed && (task.criteria.is_empty() || task.verified());

    let mut out = String::new();

    // First line: what `preview()` (`dsh/src/cron/store/claim.rs`) shows in
    // `cron history`'s detail column, so it has to stand alone as a one-line
    // verdict.
    match (task.status, non_empty_answer) {
        (TaskStatus::Completed, Some(answer)) => {
            out.push_str("completed: ");
            out.push_str(&clamp_chars(first_line(answer), HEADLINE_CHARS));
        }
        (status, _) => {
            out.push_str(status_label(status));
            if !task.criteria.is_empty() {
                out.push_str(&format!(
                    " (criteria {}/{})",
                    task.criteria.iter().filter(|c| c.passed).count(),
                    task.criteria.len()
                ));
            }
        }
    }
    out.push('\n');

    out.push_str("goal: ");
    out.push_str(&clamp_chars(&task.goal.replace('\n', " "), GOAL_CHARS));
    out.push('\n');

    out.push_str(&format!(
        "status: {}  succeeded: {succeeded}\n",
        status_label(task.status)
    ));
    out.push_str(&format!(
        "tokens: {}/{}  elapsed: {}s/{}s\n",
        task.tokens_used,
        task.token_budget,
        task.elapsed_ms / 1000,
        task.time_budget_ms / 1000
    ));
    if let Some(reason) = &task.stop_reason {
        out.push_str("stop_reason: ");
        out.push_str(&clamp_chars(reason, STOP_REASON_CHARS));
        out.push('\n');
    }

    if !task.criteria.is_empty() {
        out.push_str("criteria:\n");
        for criterion in &task.criteria {
            let mark = if criterion.passed { 'x' } else { ' ' };
            out.push_str(&format!(
                "  [{mark}] {}",
                clamp_chars(&criterion.criterion, CRITERION_CHARS)
            ));
            if let Some(event) = criterion.evidence_event {
                out.push_str(&format!("  (event {event})"));
            }
            out.push('\n');
        }
    }

    if !task.progress.is_empty() {
        out.push_str("progress: ");
        out.push_str(&clamp_chars(&task.progress, PROGRESS_CHARS));
        out.push('\n');
    }

    let stats = tool_stats(events);
    if stats.calls > 0 {
        let mut by_name = stats.by_name.clone();
        by_name.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let histogram = by_name
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "tools: {} call(s), {} failed - {histogram}\n",
            stats.calls, stats.failed
        ));
        if let Some((name, result)) = &stats.last_failure {
            out.push_str(&format!("last failure: {name} -> {result}\n"));
        }
    }

    match non_empty_answer {
        Some(answer) => {
            out.push_str("--- final answer ---\n");
            out.push_str(&clamp_chars(answer, FINAL_ANSWER_CHARS));
            out.push('\n');
        }
        None => out.push_str("(no final answer recorded)\n"),
    }

    clamp_bytes(&out, SUMMARY_BUDGET_BYTES)
}

#[cfg(test)]
mod tests;
