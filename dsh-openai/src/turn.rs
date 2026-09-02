//! Interpretation of a single chat completion response.
//!
//! Both agent loops - the `!` chat runtime and the shell-side AI service -
//! have to decide the same four things from a response: run tools, answer,
//! stop because the provider cut the reply short, or recognise that the model
//! produced nothing usable. Keeping that in one place is what stops the two
//! loops from drifting apart again.

use serde_json::Value;

pub const FINISH_REASON_LENGTH: &str = "length";
pub const FINISH_REASON_CONTENT_FILTER: &str = "content_filter";

/// What the model asked for in one turn.
#[derive(Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Tool calls to run before asking again.
    ToolCalls(Vec<Value>),
    /// A final answer.
    Answer(String),
    /// The provider stopped early.
    Cut {
        finish_reason: String,
        partial: Option<String>,
    },
    /// Neither a tool call nor an answer.
    Stalled,
}

#[derive(Debug)]
pub struct Turn {
    /// The assistant message, when it is worth keeping in the history.
    ///
    /// A message with neither tool calls nor content is left out: some
    /// providers reject it when it is replayed.
    pub assistant_message: Option<Value>,
    /// Text the model emitted alongside its tool calls.
    pub interim_text: Option<String>,
    pub outcome: TurnOutcome,
}

pub fn interpret_response(response: &Value) -> Result<Turn, String> {
    let choice = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or_else(|| format!("response contained no choices: {response}"))?;

    let assistant_message = choice
        .get("message")
        .cloned()
        .ok_or_else(|| format!("response missing assistant message: {response}"))?;

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    let tool_calls = assistant_message
        .get("tool_calls")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let content = extract_message_content(&assistant_message);

    let keep_message = !tool_calls.is_empty() || content.is_some();
    let stored_message = keep_message.then(|| assistant_message.clone());

    if !tool_calls.is_empty() {
        return Ok(Turn {
            assistant_message: stored_message,
            interim_text: content,
            outcome: TurnOutcome::ToolCalls(tool_calls),
        });
    }

    if finish_reason == FINISH_REASON_LENGTH || finish_reason == FINISH_REASON_CONTENT_FILTER {
        return Ok(Turn {
            assistant_message: stored_message,
            interim_text: None,
            outcome: TurnOutcome::Cut {
                finish_reason,
                partial: content,
            },
        });
    }

    let outcome = match content {
        Some(content) => TurnOutcome::Answer(content),
        None => TurnOutcome::Stalled,
    };

    Ok(Turn {
        assistant_message: stored_message,
        interim_text: None,
        outcome,
    })
}

/// The final text of a single-shot request, or why there is none.
///
/// A caller that offers no tools still has to face the same four outcomes, and
/// the ones that read `choices[0].message.content` by hand missed two of them:
/// a reply cut off by `finish_reason=length` came back as a complete answer, so
/// a half-written commit message looked like a finished one.
pub fn answer_text(response: &Value) -> Result<String, String> {
    match interpret_response(response)?.outcome {
        TurnOutcome::Answer(content) => Ok(content),
        TurnOutcome::Cut {
            finish_reason,
            partial,
        } => Err(describe_cut(&finish_reason, partial.as_deref())),
        TurnOutcome::Stalled => Err("the model returned no answer".to_string()),
        TurnOutcome::ToolCalls(_) => {
            Err("the model asked for a tool, but this request offers none".to_string())
        }
    }
}

/// Read the text of a message, tolerating the array content shape.
pub fn extract_message_content(message: &Value) -> Option<String> {
    let content = message.get("content")?;

    let mut segments = Vec::new();
    collect_text_segments(content, &mut segments);

    let combined = segments.join("");
    if combined.trim().is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn collect_text_segments(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.is_empty() => {
            out.push(text.to_string());
        }
        Value::Array(items) => {
            for item in items {
                collect_text_segments(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text") {
                collect_text_segments(text, out);
            }
            if let Some(content) = map.get("content") {
                collect_text_segments(content, out);
            }
            if let Some(value_field) = map.get("value") {
                collect_text_segments(value_field, out);
            }
        }
        _ => {}
    }
}

/// Limits both agent loops must agree on.
///
/// The `!` chat runtime and the shell-side AI service each drive their own
/// loop - one synchronous over the builtin tools, one asynchronous over MCP -
/// because their execution models genuinely differ. What must *not* differ is
/// the policy: they used to allow 100 versus 10 iterations and 8192 versus 4096
/// characters of tool output, which meant the same conversation behaved
/// differently depending on which entry point the user reached it through.
pub mod limits {
    /// Tool round trips one request may make before it is abandoned.
    pub const MAX_TOOL_ITERATIONS: usize = 100;
    /// Iterations allowed to a shell-side feature, which asks a bounded
    /// question and should not be able to run away.
    pub const MAX_ASSIST_ITERATIONS: usize = 10;
    /// Ceiling on a single tool result handed back to the model.
    pub const MAX_TOOL_OUTPUT_CHARS: usize = 8192;
}

/// What to do about a turn that produced neither a tool call nor an answer.
pub enum StallAction {
    /// Push this as a user message and ask again.
    Nudge(&'static str),
    /// Stop; the model has now failed twice in a row.
    GiveUp(&'static str),
}

/// Decide how to handle a stall, given how many have happened in a row.
///
/// Resending an identical request only burns the budget, so there is exactly
/// one retry. Shared so the two loops cannot drift on the count or the wording.
pub fn handle_stall(stalled_rounds: usize) -> StallAction {
    if stalled_rounds > 1 {
        StallAction::GiveUp("the model returned neither a tool call nor an answer twice in a row")
    } else {
        StallAction::Nudge(
            "Your last reply contained neither a tool call nor an answer. Either call a tool or answer the question now.",
        )
    }
}

/// Smallest budget worth splitting into a head and a tail.
const MIN_MIDDLE_TRUNCATION_BUDGET: usize = 64;

/// Truncate `text` to `max_chars` while keeping both ends.
///
/// Compiler errors, test failures and stack traces live at the *end* of a
/// command's output, so a head-only cut hides the very thing the model has to
/// react to.
pub fn truncate_middle(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    if max_chars < MIN_MIDDLE_TRUNCATION_BUDGET {
        let end = text.floor_char_boundary(max_chars);
        let omitted = text.len() - end;
        return format!("{}\n... (truncated {omitted} characters)", &text[..end]);
    }

    let head_budget = max_chars / 2;
    let tail_budget = max_chars - head_budget;
    let head_end = text.floor_char_boundary(head_budget);
    let tail_start = text.ceil_char_boundary(text.len() - tail_budget);
    let omitted = tail_start - head_end;

    format!(
        "{}\n... (truncated {omitted} characters from the middle) ...\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

/// Describe a `Cut` outcome for the user.
pub fn describe_cut(finish_reason: &str, partial: Option<&str>) -> String {
    match (finish_reason, partial) {
        (FINISH_REASON_LENGTH, Some(partial)) => format!(
            "the answer was cut off by the model output limit (finish_reason=length). Partial answer:\n{partial}"
        ),
        (FINISH_REASON_LENGTH, None) => {
            "the model hit its output limit before producing an answer (finish_reason=length)"
                .to_string()
        }
        (FINISH_REASON_CONTENT_FILTER, _) => {
            "the response was blocked by the provider content filter".to_string()
        }
        (other, _) => format!("the provider stopped early (finish_reason={other})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(message: Value, finish_reason: &str) -> Value {
        json!({ "choices": [{ "message": message, "finish_reason": finish_reason }] })
    }

    #[test]
    fn tool_calls_are_returned_with_any_interim_text() {
        let message = json!({
            "role": "assistant",
            "content": "Looking at the parser first.",
            "tool_calls": [{"id": "1", "function": {"name": "ls", "arguments": "{}"}}]
        });

        let turn = interpret_response(&response(message.clone(), "tool_calls")).unwrap();

        assert_eq!(
            turn.interim_text.as_deref(),
            Some("Looking at the parser first.")
        );
        assert_eq!(turn.assistant_message, Some(message));
        match turn.outcome {
            TurnOutcome::ToolCalls(calls) => assert_eq!(calls.len(), 1),
            other => panic!("expected tool calls, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_reply_is_an_answer() {
        let turn = interpret_response(&response(json!({"content": "done"}), "stop")).unwrap();
        assert_eq!(turn.outcome, TurnOutcome::Answer("done".to_string()));
    }

    #[test]
    fn array_content_is_read_as_an_answer() {
        let message = json!({"content": [{"type": "text", "text": {"value": "hi"}}]});
        let turn = interpret_response(&response(message, "stop")).unwrap();
        assert_eq!(turn.outcome, TurnOutcome::Answer("hi".to_string()));
    }

    #[test]
    fn an_empty_tool_call_array_is_a_stall() {
        // The old loops re-sent the identical request until the iteration cap.
        let message = json!({"role": "assistant", "tool_calls": []});
        let turn = interpret_response(&response(message, "tool_calls")).unwrap();

        assert_eq!(turn.outcome, TurnOutcome::Stalled);
        assert!(turn.assistant_message.is_none());
    }

    #[test]
    fn a_truncated_reply_reports_the_partial_answer() {
        let turn = interpret_response(&response(json!({"content": "half"}), "length")).unwrap();

        assert_eq!(
            turn.outcome,
            TurnOutcome::Cut {
                finish_reason: "length".to_string(),
                partial: Some("half".to_string())
            }
        );
        assert!(describe_cut("length", Some("half")).contains("half"));
    }

    #[test]
    fn a_filtered_reply_is_reported() {
        let turn =
            interpret_response(&response(json!({"content": null}), "content_filter")).unwrap();

        assert!(matches!(turn.outcome, TurnOutcome::Cut { .. }));
        assert!(describe_cut("content_filter", None).contains("content filter"));
    }

    #[test]
    fn answer_text_reports_a_truncated_reply_instead_of_returning_it() {
        let err = answer_text(&response(json!({"content": "half a commit mes"}), "length"))
            .expect_err("a cut reply is not an answer");
        assert!(err.contains("cut off"));
    }

    #[test]
    fn answer_text_reads_the_array_content_shape() {
        let message = json!({"content": [{"type": "text", "text": "feat: x"}]});
        assert_eq!(
            answer_text(&response(message, "stop")).unwrap(),
            "feat: x".to_string()
        );
    }

    #[test]
    fn a_response_without_choices_is_an_error() {
        assert!(interpret_response(&json!({"object": "error"})).is_err());
    }

    #[test]
    fn truncate_middle_does_not_split_multi_byte_chars() {
        // Both cut points land inside a 4-byte character.
        let text = "\u{1F980}".repeat(200);
        let truncated = truncate_middle(&text, 401);

        assert!(truncated.contains("truncated"));
        for segment in truncated.split('\n') {
            if segment.starts_with("...") {
                continue;
            }
            assert!(
                segment.chars().all(|c| c == '\u{1F980}'),
                "split char in {segment:?}"
            );
        }
    }

    #[test]
    fn truncate_middle_falls_back_to_head_for_tiny_budgets() {
        let text = "a".repeat(100);
        let truncated = truncate_middle(&text, 16);

        assert!(truncated.starts_with("aaaa"));
        assert!(truncated.contains("... (truncated 84 characters)"));
    }
}
