//! `ConversationManager`: the buffered chat history for one `!` conversation
//! (or one persistent task), its deterministic compaction pass
//! (`compact_buffer`/`superseded_tool_indices`), and the paid summarization
//! fallback (`perform_summary`). The free functions below it are the
//! transcript-shape helpers `perform_summary` and `reflect` share.
use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ConversationManager {
    pub(super) summary: Option<String>,
    pub(super) buffer: Vec<Value>,
    pub(super) buffer_chars: usize,
    /// Prompt tokens the provider reported for the most recent request.
    pub(super) last_prompt_tokens: u64,
    /// Ceiling for that figure before the conversation is summarized.
    pub(super) prompt_token_budget: u64,
    /// Usage billed to the current turn, accumulated locally.
    pub(super) turn_usage: usage::TokenUsage,
    /// System prompt (fixed) - index 0
    /// First user message (pinned) - index 1
    pub(super) pinned_messages: Vec<Value>,
    /// Where this turn started, recorded when the turn continued a stored
    /// conversation.
    ///
    /// A turn that fails is rewound to here instead of throwing the whole
    /// conversation away: the prefix below `buffer_index` was left by a turn
    /// that completed (`session::store` only ever saves one of those), so
    /// truncating to it is guaranteed to leave every `tool_calls` message
    /// paired with its results - unlike an arbitrary cut point. `None` means
    /// there is nothing to rewind to: a brand new conversation, or a restored
    /// task checkpoint (agent turns are never rewound; `session_ttl` is
    /// always `None` for them, so nothing here is read).
    #[serde(default)]
    pub(super) turn_mark: Option<TurnStart>,
}

/// What `rewind_to_turn_start` restores.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct TurnStart {
    /// Index into `buffer` where this turn's first message was added.
    pub(super) buffer_index: usize,
    /// `summary` as it stood before this turn. `perform_summary` can advance
    /// `summary` mid-turn if this turn's own messages pushed the buffer past
    /// `should_summarize`'s threshold, so a rewind that only truncated
    /// `buffer` would leave a discarded turn's actions described in every
    /// later request via the "Previous Conversation Summary" block.
    pub(super) summary: Option<String>,
}

impl ConversationManager {
    pub(super) fn new(system_prompt: Value, first_user_message: Value) -> Self {
        Self {
            summary: None,
            buffer: Vec::new(),
            buffer_chars: 0,
            last_prompt_tokens: 0,
            prompt_token_budget: DEFAULT_CONTEXT_TOKEN_BUDGET,
            turn_usage: usage::TokenUsage::default(),
            pinned_messages: vec![system_prompt, first_user_message],
            turn_mark: None,
        }
    }

    pub(super) fn add_message(&mut self, message: Value) {
        self.buffer_chars += message_serialized_len(&message);
        self.buffer.push(message);
    }

    pub(super) fn buffer_size_chars(&self) -> usize {
        self.buffer_chars
    }

    /// Record what the provider actually charged for the last request.
    ///
    /// The byte length of the buffer is only a proxy; the reported prompt size
    /// also covers the system prompt, the tool schemas and the summary.
    pub(super) fn note_prompt_tokens(&mut self, prompt_tokens: u64) {
        self.last_prompt_tokens = prompt_tokens;
    }

    pub(super) fn set_prompt_token_budget(&mut self, budget: u64) {
        self.prompt_token_budget = budget;
    }

    /// Start a fresh usage tally for a new turn on a carried conversation.
    pub(super) fn begin_turn(&mut self) {
        self.turn_usage = usage::TokenUsage::default();
    }

    /// Remember where this turn starts, before its first message is added.
    pub(super) fn mark_turn_start(&mut self) {
        self.turn_mark = Some(TurnStart {
            buffer_index: self.buffer.len(),
            summary: self.summary.clone(),
        });
    }

    /// Drop everything this turn added, leaving the conversation as it was
    /// when the last completed turn stored it - including undoing any
    /// mid-turn summarization. Returns `false` when there was no mark to
    /// rewind to (a brand new conversation, or a restored task checkpoint),
    /// in which case nothing is touched.
    pub(super) fn rewind_to_turn_start(&mut self) -> bool {
        let Some(start) = self.turn_mark.take() else {
            return false;
        };
        if start.buffer_index < self.buffer.len() {
            self.buffer.truncate(start.buffer_index);
            self.buffer_chars = sum_message_lengths(&self.buffer);
        }
        self.summary = start.summary;
        // The measured prompt size describes the larger request that just
        // failed. Left in place, `should_summarize` stays true and the next
        // turn buys a summary for a buffer that has already shrunk back down -
        // the same reason `perform_summary` clears it after shrinking the
        // buffer its own way.
        self.last_prompt_tokens = 0;
        true
    }

    /// Tell the model a rewound turn's tool calls may already have taken
    /// effect. Skipped when the buffer already ends with this exact notice
    /// from a `system` message, so a run of consecutive failures does not
    /// pile up identical notices. Checking `role` too (not just `content`)
    /// means a coincidental match in a `tool`/`assistant`/`user` message can
    /// never suppress a genuinely new notice.
    pub(super) fn note_turn_rewound(&mut self) {
        let already_noted = self.buffer.last().is_some_and(|message| {
            message.get("role").and_then(Value::as_str) == Some("system")
                && message.get("content").and_then(Value::as_str) == Some(REWIND_NOTICE)
        });
        if !already_noted {
            self.add_message(json!({ "role": "system", "content": REWIND_NOTICE }));
        }
    }

    /// Drop `retain_start` messages from the front, keeping `turn_mark`
    /// aligned with what remains. Used by `perform_summary`, so a turn that
    /// fails after summarizing still rewinds correctly. The mark's captured
    /// `summary` snapshot is left untouched - it is the value from *before*
    /// this turn started, and must survive whatever `perform_summary` does to
    /// the live `self.summary` during the turn.
    pub(super) fn drop_buffer_prefix(&mut self, retain_start: usize) {
        self.buffer = self.buffer.split_off(retain_start);
        self.buffer_chars = sum_message_lengths(&self.buffer);
        if let Some(start) = &mut self.turn_mark {
            start.buffer_index = start.buffer_index.saturating_sub(retain_start);
        }
    }

    pub(super) fn last_prompt_tokens(&self) -> u64 {
        self.last_prompt_tokens
    }

    pub(super) fn prompt_token_budget(&self) -> u64 {
        self.prompt_token_budget
    }

    pub(super) fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    pub(super) fn should_summarize(&self) -> bool {
        self.buffer_size_chars() > MAX_BUFFER_CHARS
            || self.last_prompt_tokens > self.prompt_token_budget
    }

    /// Shrink the buffer without paying a model to do it.
    ///
    /// Summarizing costs a whole extra request, and most of what makes a long
    /// agent conversation large is not conversation at all: it is tool output
    /// the model has already acted on, and files it read more than once. Both
    /// can be dropped by rule.
    ///
    /// Only the `content` of a `tool` message is replaced, never the message
    /// itself. A `tool` message is only valid directly after the assistant
    /// message that asked for it, so removing one would leave the request
    /// dangling and the API answers that with a 400.
    ///
    /// Returns the number of characters reclaimed.
    pub(super) fn compact_buffer(&mut self) -> usize {
        let before = self.buffer_chars;

        for index in self.superseded_tool_indices() {
            // The stub is not free. Replacing a two-byte "ok" with a sentence
            // naming the call makes the buffer *larger*, which is the opposite
            // of the job.
            if message_serialized_len(&self.buffer[index]) <= MIN_ELIDABLE_TOOL_CHARS {
                continue;
            }
            let label = tool_call_label(&self.buffer, index)
                .unwrap_or_else(|| "identical call".to_string());
            replace_tool_content(
                &mut self.buffer[index],
                &format!("(superseded by a later {label}; its newer result is below)"),
            );
        }

        // Everything before the last few exchanges is history the model has
        // already folded into what it did next.
        let keep_from = retain_boundary(&self.buffer, RECENT_BUFFER_MESSAGES_KEPT);
        for index in 0..keep_from {
            if message_role(&self.buffer[index]) != Some("tool") {
                continue;
            }
            let size = message_serialized_len(&self.buffer[index]);
            if size <= MIN_ELIDABLE_TOOL_CHARS {
                continue;
            }
            let label =
                tool_call_label(&self.buffer, index).unwrap_or_else(|| "tool result".to_string());
            replace_tool_content(
                &mut self.buffer[index],
                &format!("(elided: {label}, {size} bytes; call it again if you need it)"),
            );
        }

        self.buffer_chars = sum_message_lengths(&self.buffer);
        before.saturating_sub(self.buffer_chars)
    }

    /// Indices of tool results that a later identical call has replaced.
    ///
    /// Reading the same file twice used to keep both copies in the request for
    /// the rest of the conversation.
    pub(super) fn superseded_tool_indices(&self) -> Vec<usize> {
        let mut latest: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut superseded = Vec::new();

        for index in 0..self.buffer.len() {
            let Some(key) = tool_call_signature(&self.buffer, index) else {
                continue;
            };
            if let Some(previous) = latest.insert(key, index) {
                superseded.push(previous);
            }
        }

        superseded
    }

    pub(super) fn perform_summary(
        &mut self,
        client: &dyn ChatClient,
        proxy: &mut dyn ChatToolHost,
        model_override: Option<String>,
    ) -> Result<(), String> {
        let _spinner = SpinnerGuard::start("Summarizing conversation history...");

        // Determine which model to use for summarization:
        // 1. Check for AI_SUMMARY_MODEL environment variable
        // 2. Fall back to the main chat model (model_override or default)
        let summary_model = proxy
            .get_var(SUMMARY_MODEL_KEY)
            .or_else(|| std::env::var(SUMMARY_MODEL_KEY).ok())
            .or(model_override);

        let mut summary_messages = Vec::new();
        summary_messages.push(json!({
            "role": "system",
            "content": "You are a conversation summarizer. Your task is to update the summary of a technical conversation between a user and an AI DevOps agent. 
            
            Inputs:
            1. Current Summary (if any)
            2. Recent Messages (to be summarized)

            Output:
            A single, concise paragraph summarizing the entire history including the new messages. 
            - PRESERVE key technical details: file names, function names, error messages, and what actions were taken.
            - OMIT trivial chatter.
            - FOCUS on the state of the system and the progress of the task."
        }));

        let current_summary_text = self.summary.as_deref().unwrap_or("None");
        let buffer_text = flatten_conversation(&self.buffer, MAX_SUMMARY_TOOL_CHARS);

        summary_messages.push(json!({
            "role": "user",
            "content": format!("Current Summary:\n{current_summary_text}\n\nRecent Messages to Integrate:\n{buffer_text}")
        }));

        // Send request to summarization model
        // No `max_completion_tokens`: on a reasoning model that budget also
        // covers hidden reasoning, so a tight cap returns finish_reason=length
        // with no summary at all.
        let options = ChatRequestOptions::new()
            .with_temperature(Some(0.3)) // Lower temperature for consistent summarization
            .with_model(summary_model);
        let response = client
            .send_chat_cancellable(&summary_messages, &options, &|| task_cancelled(proxy))
            .map_err(|e| format!("Summarization failed: {e}"))?;
        self.turn_usage.add_response(&response);
        if let Some(runtime) = proxy.agent_runtime() {
            let mut runtime = runtime.lock();
            runtime
                .checkpoint(
                    serde_json::to_value(&*self).map_err(|e| e.to_string())?,
                    self.turn_usage.total_tokens(),
                )
                .map_err(|e| e.to_string())?;
            if usage::TokenUsage::from_response(&response).is_none() {
                return Err("agent: summary provider omitted token usage".into());
            }
            if runtime.stopped() {
                return Err("agent: task stopped or budget exhausted during summary".into());
            }
        }

        let new_summary = summary_from_response(&response)?;

        // A later rewind (`rewind_to_turn_start`) falls back to
        // `turn_mark.summary` - the summary as it stood *before this turn
        // began*. Left there, it would omit whatever raw history
        // `drop_buffer_prefix` below is about to fold into `new_summary`:
        // that raw text is about to stop existing anywhere else once it
        // drops, so reverting to the old summary on a later rewind would
        // silently and permanently lose it - not just this turn's own
        // actions, but real conversation history from before it even
        // started. Advancing the mark's own snapshot to `new_summary`
        // avoids that, at the cost of a narrower, self-correcting downside:
        // if this turn's own messages already number more than
        // `RETAIN_AFTER_SUMMARY` (so this drop reaches into them too), a
        // later rewind can leave a trace of them in the summary until the
        // next round of summarization revises it away - preferred over
        // losing conversation history outright.
        if let Some(start) = &mut self.turn_mark {
            start.summary = Some(new_summary.clone());
        }

        // Update state: keep most recent messages to maintain tool_call/result continuity
        const RETAIN_AFTER_SUMMARY: usize = 6; // Keep last ~3 exchanges (assistant+tool pairs)
        let retain_start = retain_boundary(&self.buffer, RETAIN_AFTER_SUMMARY);
        self.drop_buffer_prefix(retain_start);
        self.summary = Some(new_summary);
        // The measured prompt size describes the request we just replaced. Left
        // in place it keeps `should_summarize` true, and the caller's
        // `while` loop bills a summarization request per iteration forever.
        self.last_prompt_tokens = 0;

        Ok(())
    }

    /// Assemble the request, stable prefix first.
    ///
    /// Providers cache the longest common prefix of a request, so nothing
    /// volatile may appear before the conversation. The environment snapshot
    /// used to sit at index 1, which invalidated the cache for the whole
    /// conversation every time a file or the git branch changed.
    pub(super) fn build_messages_for_chat(&self, dynamic_context: Value) -> Vec<Value> {
        let mut messages = Vec::new();

        // System prompt (index 0)
        messages.push(self.pinned_messages[0].clone());

        // First user message (index 1, pinned) - the original goal
        messages.push(self.pinned_messages[1].clone());

        // Summary if present
        if let Some(summary) = &self.summary {
            messages.push(json!({
                "role": "system",
                "content": format!("## Previous Conversation Summary\nThe following is a summary of the earlier conversation. Use this to maintain context.\n\n{summary}")
            }));
        }

        // Buffer (recent messages)
        messages.extend(self.buffer.clone());

        // Volatile environment snapshot last.
        messages.push(dynamic_context);
        messages
    }
}

/// Flatten the buffer to `"role: content [Called: tool(args)]"` lines, one
/// per message, joined by a blank line.
///
/// Shared by `perform_summary` (the paid conversation summary) and
/// `reflect` (the optional turn-end skill reviewer) so the two read the
/// exact same shape of transcript - the reviewer is not a second, slightly
/// different idea of "what happened this turn".
pub(super) fn flatten_conversation(buffer: &[Value], max_tool_chars: usize) -> String {
    buffer
        .iter()
        .map(|msg| {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let mut content = extract_message_content(msg).unwrap_or_default();
            if role == "tool" && content.len() > max_tool_chars {
                // The reader needs the gist, not the whole build log.
                let end = content.floor_char_boundary(max_tool_chars);
                content = format!("{}... (truncated)", &content[..end]);
            }

            // Include tool_calls information if present
            let tool_calls_desc = msg
                .get("tool_calls")
                .and_then(|tc| tc.as_array())
                .map(|calls| {
                    let tool_names: Vec<String> = calls
                        .iter()
                        .filter_map(|c| {
                            let name = c
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())?;
                            let args = c
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}");
                            Some(format!("{name}({args})"))
                        })
                        .collect();
                    if tool_names.is_empty() {
                        String::new()
                    } else {
                        format!(" [Called: {}]", tool_names.join(", "))
                    }
                })
                .unwrap_or_default();

            format!("{role}: {content}{tool_calls_desc}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Where to cut the buffer so that `retain` messages survive a summary.
///
/// A `tool` message is only valid immediately after the assistant message that
/// requested it. Cutting between the two leaves an orphan that the API rejects
/// with a 400, which used to surface as a failure right after every summary of
/// a long session. Walking backwards keeps at most one extra exchange.
pub(super) fn retain_boundary(buffer: &[Value], retain: usize) -> usize {
    let mut start = buffer.len().saturating_sub(retain);
    while start > 0 && message_role(&buffer[start]) == Some("tool") {
        start -= 1;
    }
    start
}

/// The call a `tool` message answers: its function name and its arguments.
///
/// Derived from the assistant message that requested it rather than stored on
/// the tool message, because everything on that message is sent to the
/// provider and an unknown field is something an endpoint may reject.
pub(super) fn tool_call_target(buffer: &[Value], index: usize) -> Option<(String, String)> {
    let message = buffer.get(index)?;
    if message_role(message)? != "tool" {
        return None;
    }
    let call_id = message.get("tool_call_id").and_then(Value::as_str)?;

    // The request sits in the nearest preceding assistant message.
    buffer[..index].iter().rev().find_map(|candidate| {
        let calls = candidate.get("tool_calls")?.as_array()?;
        let call = calls
            .iter()
            .find(|call| call.get("id").and_then(Value::as_str) == Some(call_id))?;
        let function = call.get("function")?;
        let name = function.get("name")?.as_str()?.to_string();
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_string();
        Some((name, arguments))
    })
}

/// Two calls with the same name and arguments return the same thing, so only
/// the newer one is worth carrying.
pub(super) fn tool_call_signature(buffer: &[Value], index: usize) -> Option<String> {
    tool_call_target(buffer, index).map(|(name, arguments)| format!("{name}({arguments})"))
}

/// A short label for the stub left where a result used to be.
///
/// The stub has to name the call, or the model cannot judge whether re-running
/// it is worth a turn.
pub(super) fn tool_call_label(buffer: &[Value], index: usize) -> Option<String> {
    let (name, arguments) = tool_call_target(buffer, index)?;
    Some(format!("{name}({})", shorten_arguments(&arguments)))
}

/// Enough of the arguments to identify the call, and no more.
pub(super) fn shorten_arguments(arguments: &str) -> String {
    pub(super) const MAX: usize = 80;
    let trimmed = arguments.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let end = trimmed.floor_char_boundary(MAX);
    format!("{}...", &trimmed[..end])
}

/// Swap a tool result's content for a stub, leaving the message in place.
pub(super) fn replace_tool_content(message: &mut Value, stub: &str) {
    if let Some(map) = message.as_object_mut() {
        map.insert("content".into(), Value::String(stub.to_string()));
    }
}

pub(super) fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(|role| role.as_str())
}

pub(super) fn message_serialized_len(message: &Value) -> usize {
    message.to_string().len()
}

pub(super) fn sum_message_lengths(messages: &[Value]) -> usize {
    messages.iter().map(message_serialized_len).sum()
}
