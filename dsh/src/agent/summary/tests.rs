use super::*;
use dsh_types::agent::{TaskGrant, Verification};
use serde_json::json;

fn task() -> AgentTask {
    AgentTask {
        id: "task-1".to_string(),
        goal: "summarise the PRs".to_string(),
        root: "/tmp".into(),
        status: TaskStatus::Completed,
        grant: TaskGrant::default(),
        criteria: vec![],
        plan: vec![],
        progress: String::new(),
        tokens_used: 12_345,
        time_budget_ms: 600_000,
        elapsed_ms: 41_000,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}

fn assistant_message(text: &str) -> Value {
    json!({"role": "assistant", "content": text})
}

fn checkpoint_with(messages: Vec<Value>) -> Value {
    json!({"buffer": messages})
}

fn event(sequence: u64, kind: &str, data: Value) -> TaskEvent {
    TaskEvent {
        sequence,
        kind: kind.to_string(),
        data,
    }
}

fn tool_result_event(sequence: u64, name: &str, failed: bool, result: &str) -> TaskEvent {
    event(
        sequence,
        "tool_result",
        json!({
            "call": {"function": {"name": name, "arguments": "{}"}},
            "result": result,
            "failed": failed,
            "outcome": if failed { "failure" } else { "success" },
        }),
    )
}

#[test]
fn the_final_answer_is_taken_from_the_end_of_the_checkpoint_buffer() {
    let checkpoint = checkpoint_with(vec![
        assistant_message("first draft"),
        json!({"role": "user", "content": "no, redo it"}),
        assistant_message("the real answer"),
    ]);
    assert_eq!(
        final_answer(Some(&checkpoint)).as_deref(),
        Some("the real answer")
    );
}

#[test]
fn a_tool_call_only_assistant_message_is_not_mistaken_for_an_answer() {
    let checkpoint = checkpoint_with(vec![
        assistant_message("earlier answer"),
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "1", "function": {"name": "execute", "arguments": "{}"}}],
        }),
    ]);
    // The trailing message has no usable content, so the search must fall
    // back to the last one that does, not stop at `None` and give up.
    assert_eq!(
        final_answer(Some(&checkpoint)).as_deref(),
        Some("earlier answer")
    );
}

#[test]
fn an_array_shaped_content_is_flattened() {
    let checkpoint = checkpoint_with(vec![json!({
        "role": "assistant",
        "content": [{"type": "text", "text": "part one"}, {"type": "text", "text": " part two"}],
    })]);
    assert_eq!(
        final_answer(Some(&checkpoint)).as_deref(),
        Some("part one part two")
    );
}

#[test]
fn a_task_with_no_checkpoint_still_summarises() {
    let t = task();
    assert_eq!(final_answer(t.checkpoint.as_ref()), None);
    let text = task_summary(&t, &[]);
    assert!(text.contains("(no final answer recorded)"), "{text}");
    assert!(text.contains("goal: summarise the PRs"), "{text}");
}

#[test]
fn a_malformed_checkpoint_is_not_a_panic() {
    assert_eq!(final_answer(Some(&json!("nonsense"))), None);
    assert_eq!(final_answer(Some(&json!({"buffer": "not an array"}))), None);
    assert_eq!(final_answer(Some(&json!({"buffer": [1, 2, 3]}))), None);

    let mut t = task();
    t.checkpoint = Some(json!("nonsense"));
    // Must not panic.
    let _ = task_summary(&t, &[]);
}

#[test]
fn the_first_line_is_a_one_line_verdict_so_the_cron_preview_is_useful() {
    let mut t = task();
    t.checkpoint = Some(checkpoint_with(vec![assistant_message(
        "Wrote out/digest.md with 7 open PRs.\nSome more detail on a second line.",
    )]));
    let text = task_summary(&t, &[]);
    let first_line = text.lines().next().unwrap();
    assert!(first_line.chars().count() <= HEADLINE_CHARS + "completed: ".len());
    assert!(
        first_line.starts_with("completed: Wrote out/digest.md"),
        "{first_line}"
    );
    assert!(!first_line.contains("second line"), "{first_line}");
}

#[test]
fn criteria_show_which_ones_passed() {
    let mut t = task();
    t.criteria = vec![
        Verification {
            criterion: "a".to_string(),
            evidence_event: Some(3),
            passed: true,
        },
        Verification {
            criterion: "b".to_string(),
            evidence_event: None,
            passed: false,
        },
    ];
    let text = task_summary(&t, &[]);
    assert!(text.contains("[x] a  (event 3)"), "{text}");
    assert!(text.contains("[ ] b\n"), "{text}");
}

#[test]
fn the_last_failed_tool_result_is_the_one_that_is_named() {
    let events = vec![
        tool_result_event(1, "execute", true, "first failure"),
        tool_result_event(2, "execute", false, "ok now"),
        tool_result_event(3, "edit", true, "second failure"),
    ];
    let text = task_summary(&task(), &events);
    assert!(text.contains("3 call(s), 2 failed"), "{text}");
    assert!(text.contains("execute x2"), "{text}");
    assert!(text.contains("edit x1"), "{text}");
    assert!(
        text.contains("last failure: edit -> second failure"),
        "{text}"
    );
}

#[test]
fn the_summary_stays_under_the_stream_budget_even_with_a_huge_answer() {
    let mut t = task();
    let huge = "x".repeat(1_000_000);
    t.checkpoint = Some(checkpoint_with(vec![assistant_message(&huge)]));
    let text = task_summary(&t, &[]);
    assert!(text.len() <= SUMMARY_BUDGET_BYTES, "{}", text.len());
}

#[test]
fn the_agent_digest_ignores_which_status_label_capitalisation_is_used() {
    // Not a real digest test (that lives in `cron::run_job`) - just pins
    // `status_label`'s output shape, since `cron`'s `preview()` reads the
    // first line of this module's output as its one-line verdict.
    assert_eq!(status_label(TaskStatus::Completed), "completed");
    assert_eq!(status_label(TaskStatus::InputRequired), "input-required");
}

/// A stop without a parseable hint still guides when the recorded results
/// name the refusal: the events fallback recovers the resume command.
#[test]
fn a_hintless_stop_recovers_its_fix_from_recorded_results() {
    let mut t = task();
    t.status = TaskStatus::Interrupted;
    t.stop_reason = Some("task stopped before completion (budget or interruption)".into());
    // Budgets intact, so the grant recovery below applies rather than a
    // budget warning.
    t.tokens_used = 0;
    t.elapsed_ms = 0;
    let events = vec![tool_result_event(
        1,
        "execute",
        true,
        "Error: agent: command permission required: cargo test -p foo\nPlease analyze the error and retry with corrected arguments.",
    )];
    let text = task_summary(&t, &events);
    assert!(text.contains("needs:"), "{text}");
    assert!(
        text.contains("agent resume task-1 --allow-command 'cargo test -p foo'"),
        "{text}"
    );
}

/// On an exhausted time budget the grant recovery stays out of the way: raising the
/// timeout is the fix, and a stale grant line would mislead.
#[test]
fn an_exhausted_budget_suppresses_the_events_recovery() {
    let mut t = task();
    t.status = TaskStatus::Interrupted;
    t.stop_reason = Some("task stopped before completion (time budget or interruption)".into());
    t.elapsed_ms = t.time_budget_ms;
    let events = vec![tool_result_event(
        1,
        "execute",
        true,
        "Error: agent: command permission required: cargo test -p foo\nPlease analyze the error and retry with corrected arguments.",
    )];
    let text = task_summary(&t, &events);
    assert!(!text.contains("needs:"), "{text}");
}
