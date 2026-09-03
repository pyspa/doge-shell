//! SSE framing and delta aggregation for streamed chat completions.
//!
//! The provider sends a streamed chat completion as a sequence of
//! `data: {...}\n\n` frames terminated by `data: [DONE]`. This module turns
//! that byte stream into a single `serde_json::Value` shaped exactly like a
//! non-streaming response - the same `choices[0].message` /
//! `finish_reason` / `usage` shape `turn::interpret_response` already knows
//! how to read. Nothing downstream of aggregation has to know the answer
//! was streamed.

use serde_json::{Map, Value, json};

/// The provider's end-of-stream sentinel. Not JSON, so it must be checked
/// for before attempting to parse a frame.
pub const DONE_MARKER: &str = "[DONE]";

/// Whether a decoded SSE payload is the end-of-stream sentinel.
pub fn is_done_marker(payload: &str) -> bool {
    payload.trim() == DONE_MARKER
}

/// Ceiling on `delta.tool_calls[].index` the aggregator will accept.
///
/// The index drives `Vec::resize` directly; a malformed or hostile server
/// sending an enormous index would otherwise make `DeltaAggregator` try to
/// allocate a `Vec` of that size. No real turn calls remotely this many
/// tools in parallel, so a fragment naming an index past this is dropped
/// rather than trusted.
const MAX_TOOL_CALL_INDEX: usize = 128;

/// Splits a raw SSE byte stream into event payloads.
///
/// Bytes arrive from the network in arbitrary chunks: a line can be split
/// across two calls to [`push`](Self::push), and so can a multi-byte UTF-8
/// character. Splitting is done on the raw `\n` byte before any UTF-8
/// decoding happens - `0x0A` never appears inside a multi-byte UTF-8
/// sequence, only as a literal newline - so a line is decoded only once it
/// is known to be complete. A partial line at the end of a chunk simply
/// waits in the buffer for more bytes.
#[derive(Debug, Default)]
pub struct SseFrameSplitter {
    pending: Vec<u8>,
    current_data_lines: Vec<String>,
    saw_any_data_line: bool,
}

impl SseFrameSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes, returning any complete event payloads found so far.
    ///
    /// A payload is every `data:` line of one event (terminated by a blank
    /// line), joined by `\n` per the SSE spec - the provider always sends
    /// one `data:` line per event, but a proxy that reflows the body should
    /// not break decoding. Comment lines (`:...`) and any other field name
    /// (`event:`, `id:`, `retry:`) are ignored: this protocol only uses
    /// `data:`.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();

        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let mut line_bytes: Vec<u8> = self.pending.drain(..=pos).collect();
            line_bytes.pop(); // the '\n' itself
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);

            if line.is_empty() {
                if let Some(event) = self.take_event() {
                    events.push(event);
                }
                continue;
            }

            if let Some(rest) = line.strip_prefix("data:") {
                let value = rest.strip_prefix(' ').unwrap_or(rest);
                self.current_data_lines.push(value.to_string());
                self.saw_any_data_line = true;
            }
            // Comment lines (starting with ':') and other SSE fields carry
            // no payload for this API and are dropped.
        }

        events
    }

    /// Flush a final event that never received a trailing blank line.
    ///
    /// A well-behaved server always terminates the stream cleanly, but
    /// treating a missing final blank line as "no event" would silently
    /// drop the last chunk of a reply that a proxy cut off mid-response.
    pub fn finish(&mut self) -> Vec<String> {
        self.take_event().into_iter().collect()
    }

    fn take_event(&mut self) -> Option<String> {
        if !self.saw_any_data_line {
            return None;
        }
        let payload = self.current_data_lines.join("\n");
        self.current_data_lines.clear();
        self.saw_any_data_line = false;
        Some(payload)
    }
}

/// One tool call as it accumulates across `delta.tool_calls[]` fragments.
///
/// The provider sends `id` and `function.name` only in the first fragment
/// for a given `index`, and splits `function.arguments` across many
/// fragments - sometimes one character at a time. Losing the `id` is fatal
/// downstream: `chatgpt.rs`'s history compaction looks up a tool result by
/// walking back to the assistant message whose `tool_calls[].id` matches.
#[derive(Debug, Default, Clone)]
struct ToolCallAccumulator {
    id: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Accumulates streamed chat-completion chunks into a `Value` shaped
/// exactly like a non-streaming response.
///
/// This is the seam that keeps streaming invisible to everything past it:
/// `turn::interpret_response`, the history compaction in `chatgpt.rs`, and
/// `usage::TokenUsage::from_response` all read `choices[0].message` /
/// `finish_reason` / `usage` and do not need to know whether the response
/// arrived in one piece or many.
#[derive(Debug, Default)]
pub struct DeltaAggregator {
    role: Option<String>,
    content: String,
    tool_calls: Vec<ToolCallAccumulator>,
    finish_reason: Option<String>,
    usage: Option<Value>,
}

impl DeltaAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one decoded chunk. Returns the text delta to show the user,
    /// when this chunk carried one.
    pub fn apply(&mut self, chunk: &Value) -> Option<String> {
        if let Some(usage) = chunk.get("usage")
            && !usage.is_null()
        {
            self.usage = Some(usage.clone());
        }

        let choice = chunk.get("choices").and_then(|choices| choices.get(0))?;

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_string());
        }

        let delta = choice.get("delta")?;

        if let Some(role) = delta.get("role").and_then(Value::as_str) {
            self.role = Some(role.to_string());
        }

        let text_delta = delta.get("content").and_then(Value::as_str).map(|text| {
            self.content.push_str(text);
            text.to_string()
        });

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                self.apply_tool_call_delta(tool_call);
            }
        }

        text_delta
    }

    fn apply_tool_call_delta(&mut self, tool_call: &Value) {
        let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        if index >= MAX_TOOL_CALL_INDEX {
            return;
        }
        if self.tool_calls.len() <= index {
            self.tool_calls
                .resize(index + 1, ToolCallAccumulator::default());
        }
        let acc = &mut self.tool_calls[index];

        if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
            acc.id = Some(id.to_string());
        }
        if let Some(kind) = tool_call.get("type").and_then(Value::as_str) {
            acc.kind = Some(kind.to_string());
        }
        if let Some(function) = tool_call.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                acc.name.get_or_insert_with(String::new).push_str(name);
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                acc.arguments.push_str(arguments);
            }
        }
    }

    /// Whether a `finish_reason` has arrived yet.
    ///
    /// A properly completed turn always carries one, in the same chunk as
    /// (or before) `[DONE]`. Its absence when the connection closes means
    /// the stream ended abnormally - a dropped connection, a proxy that cut
    /// the response short - and `finish()`'s output must not be read as a
    /// complete, successful answer.
    pub fn has_finish_reason(&self) -> bool {
        self.finish_reason.is_some()
    }

    /// Build a response `Value` shaped like a non-streaming chat completion.
    pub fn finish(self) -> Value {
        let mut message = Map::new();
        message.insert(
            "role".to_string(),
            json!(self.role.unwrap_or_else(|| "assistant".to_string())),
        );
        message.insert(
            "content".to_string(),
            if self.content.is_empty() {
                Value::Null
            } else {
                json!(self.content)
            },
        );

        if !self.tool_calls.is_empty() {
            let tool_calls: Vec<Value> = self
                .tool_calls
                .into_iter()
                .map(|acc| {
                    json!({
                        "id": acc.id.unwrap_or_default(),
                        "type": acc.kind.unwrap_or_else(|| "function".to_string()),
                        "function": {
                            "name": acc.name.unwrap_or_default(),
                            "arguments": acc.arguments,
                        }
                    })
                })
                .collect();
            message.insert("tool_calls".to_string(), json!(tool_calls));
        }

        let choice = json!({
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": self.finish_reason,
        });

        let mut response = Map::new();
        response.insert("choices".to_string(), json!([choice]));
        if let Some(usage) = self.usage {
            response.insert("usage".to_string(), usage);
        }

        Value::Object(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::{TurnOutcome, interpret_response};

    fn event(payload: &str) -> String {
        format!("data: {payload}\n\n")
    }

    #[test]
    fn splits_a_single_complete_event() {
        let mut splitter = SseFrameSplitter::new();
        let events = splitter.push(event("hello").as_bytes());
        assert_eq!(events, vec!["hello".to_string()]);
    }

    #[test]
    fn splits_multiple_events_in_one_push() {
        let mut splitter = SseFrameSplitter::new();
        let bytes = format!("{}{}", event("a"), event("b"));
        let events = splitter.push(bytes.as_bytes());
        assert_eq!(events, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn handles_a_line_split_across_two_chunks() {
        let mut splitter = SseFrameSplitter::new();
        assert!(splitter.push(b"data: hel").is_empty());
        let events = splitter.push(b"lo\n\n");
        assert_eq!(events, vec!["hello".to_string()]);
    }

    #[test]
    fn handles_a_multibyte_utf8_character_split_across_chunks() {
        // "日" is E6 97 A5 in UTF-8; split after the first byte.
        let full = "data: 日本語\n\n".as_bytes().to_vec();
        let (first, second) = full.split_at(7);
        let mut splitter = SseFrameSplitter::new();
        assert!(splitter.push(first).is_empty());
        let events = splitter.push(second);
        assert_eq!(events, vec!["日本語".to_string()]);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let mut splitter = SseFrameSplitter::new();
        let events = splitter.push(b"data: hello\r\n\r\n");
        assert_eq!(events, vec!["hello".to_string()]);
    }

    #[test]
    fn ignores_comment_and_unknown_field_lines() {
        let mut splitter = SseFrameSplitter::new();
        let events = splitter.push(b": keep-alive\nid: 1\nevent: message\ndata: hello\n\n");
        assert_eq!(events, vec!["hello".to_string()]);
    }

    #[test]
    fn joins_multiple_data_lines_in_one_event() {
        let mut splitter = SseFrameSplitter::new();
        let events = splitter.push(b"data: line1\ndata: line2\n\n");
        assert_eq!(events, vec!["line1\nline2".to_string()]);
    }

    #[test]
    fn recognises_the_done_marker() {
        let mut splitter = SseFrameSplitter::new();
        let events = splitter.push(event(DONE_MARKER).as_bytes());
        assert_eq!(events.len(), 1);
        assert!(is_done_marker(&events[0]));
    }

    #[test]
    fn finish_flushes_an_event_missing_its_trailing_blank_line() {
        let mut splitter = SseFrameSplitter::new();
        assert!(splitter.push(b"data: partial\n").is_empty());
        assert_eq!(splitter.finish(), vec!["partial".to_string()]);
    }

    #[test]
    fn finish_is_empty_when_nothing_is_pending() {
        let mut splitter = SseFrameSplitter::new();
        splitter.push(b"data: hello\n\n");
        assert!(splitter.finish().is_empty());
    }

    #[test]
    fn aggregator_concatenates_content_deltas() {
        let mut agg = DeltaAggregator::new();
        assert_eq!(
            agg.apply(&json!({"choices":[{"delta":{"role":"assistant","content":"Hel"}}]})),
            Some("Hel".to_string())
        );
        assert_eq!(
            agg.apply(&json!({"choices":[{"delta":{"content":"lo"}}]})),
            Some("lo".to_string())
        );
        agg.apply(&json!({"choices":[{"delta":{},"finish_reason":"stop"}]}));

        let response = agg.finish();
        let outcome = interpret_response(&response).expect("interpretable");
        match outcome.outcome {
            TurnOutcome::Answer(content) => assert_eq!(content, "Hello"),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn aggregator_reassembles_a_tool_call_split_across_many_fragments() {
        let mut agg = DeltaAggregator::new();
        agg.apply(&json!({"choices":[{"delta":{"role":"assistant","tool_calls":[
            {"index":0,"id":"call_1","type":"function","function":{"name":"exec","arguments":""}}
        ]}}]}));
        // `arguments` streamed one character at a time, as real providers do.
        for ch in ["{\"", "cmd", "\":\"", "ls", "\"}"] {
            agg.apply(&json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":ch}}
            ]}}]}));
        }
        agg.apply(&json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));

        let response = agg.finish();
        let outcome = interpret_response(&response).expect("interpretable");
        match outcome.outcome {
            TurnOutcome::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0]["id"], "call_1");
                assert_eq!(calls[0]["function"]["name"], "exec");
                assert_eq!(calls[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
    }

    #[test]
    fn aggregator_keeps_multiple_tool_calls_by_index() {
        let mut agg = DeltaAggregator::new();
        agg.apply(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_a","type":"function","function":{"name":"a","arguments":"{}"}},
            {"index":1,"id":"call_b","type":"function","function":{"name":"b","arguments":"{}"}}
        ]}}]}));

        let response = agg.finish();
        let outcome = interpret_response(&response).expect("interpretable");
        match outcome.outcome {
            TurnOutcome::ToolCalls(calls) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0]["id"], "call_a");
                assert_eq!(calls[1]["id"], "call_b");
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
    }

    /// The index drives a `Vec::resize`: a malformed or hostile server
    /// sending an enormous one must not turn into an unbounded allocation.
    #[test]
    fn aggregator_drops_a_tool_call_fragment_with_an_out_of_range_index() {
        let mut agg = DeltaAggregator::new();
        agg.apply(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_a","type":"function","function":{"name":"a","arguments":"{}"}}
        ]}}]}));
        agg.apply(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":999999999,"id":"call_evil","type":"function","function":{"name":"x","arguments":"{}"}}
        ]}}]}));

        let response = agg.finish();
        let outcome = interpret_response(&response).expect("interpretable");
        match outcome.outcome {
            TurnOutcome::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1, "the out-of-range fragment must be dropped");
                assert_eq!(calls[0]["id"], "call_a");
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
    }

    #[test]
    fn aggregator_reports_content_as_null_when_only_tool_calls_arrived() {
        let mut agg = DeltaAggregator::new();
        agg.apply(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_1","type":"function","function":{"name":"exec","arguments":"{}"}}
        ]}}]}));

        let response = agg.finish();
        let message = &response["choices"][0]["message"];
        assert!(message["content"].is_null());
    }

    #[test]
    fn aggregator_retains_usage_from_the_final_chunk() {
        let mut agg = DeltaAggregator::new();
        agg.apply(&json!({"choices":[{"delta":{"content":"hi"}}], "usage": null}));
        agg.apply(&json!({
            "choices": [],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3}
        }));

        let response = agg.finish();
        assert_eq!(response["usage"]["prompt_tokens"], 12);
        assert_eq!(response["usage"]["completion_tokens"], 3);
    }

    #[test]
    fn aggregator_reflects_a_length_cut_via_finish_reason() {
        let mut agg = DeltaAggregator::new();
        agg.apply(&json!({"choices":[{"delta":{"role":"assistant","content":"partial answer"}}]}));
        agg.apply(&json!({"choices":[{"delta":{},"finish_reason":"length"}]}));

        let response = agg.finish();
        let outcome = interpret_response(&response).expect("interpretable");
        match outcome.outcome {
            TurnOutcome::Cut {
                finish_reason,
                partial,
            } => {
                assert_eq!(finish_reason, "length");
                assert_eq!(partial.as_deref(), Some("partial answer"));
            }
            other => panic!("expected a cut, got {other:?}"),
        }
    }
}
