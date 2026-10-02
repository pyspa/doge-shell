//! Recoverable compaction: offloading tool results into the Observation Store.
//!
//! Split from conversation to keep file budgets: this holds
//! the conversation-owned offload path (add_tool_result, seen-tracking,
//! compact_buffer_report / offload_tool_result, observation footprint)
//! while conversation.rs keeps the buffered history, summarization fallback,
//! and transcript helpers.
use super::super::conversation::{
    ConversationManager, message_role, message_serialized_len, replace_tool_content,
    retain_boundary, sum_message_lengths, tool_call_label, tool_call_target,
};
use super::super::footprint::ObservationFootprint;
use super::super::settings::{MIN_ELIDABLE_TOOL_CHARS, RECENT_BUFFER_MESSAGES_KEPT};
use super::{
    ObservationReason, active_observation_references, is_observation_read_tool,
    is_observation_stub, observation_stub,
};
use serde_json::{Value, json};

/// Structured compaction report: the overall reclaimed amount plus how many
/// results became recoverable observations, fell back to ordinary elision,
/// or were protected because unseen.
#[derive(Debug, Default)]
pub(crate) struct CompactionReport {
    pub reclaimed_bytes: usize,
    pub recoverable_offloads: usize,
    pub recoverable_original_bytes: usize,
    pub fallback_elisions: usize,
    pub skipped_unseen: usize,
}

#[derive(Debug)]
enum OffloadOutcome {
    Recovered {
        original_bytes: usize,
    },
    /// Needs the historical non-recoverable stub (store full,
    /// `observation_read` result, oversized single entry).
    NeedsFallback,
    SkippedUnseen,
    SkippedSmall,
    SkippedAlready,
    SkippedNonTool,
}

impl ConversationManager {
    /// Canonical tool-result insertion: appends the provider-compatible `tool`
    /// message, updates `buffer_chars`, records the logical tool name, and
    /// marks the result unseen until a provider request successfully contains
    /// it. Preserves the exact message shape `add_message` produced.
    pub(crate) fn add_tool_result(
        &mut self,
        tool_call_id: String,
        tool_name: &str,
        content: String,
    ) {
        if !tool_name.is_empty() {
            self.tool_names
                .insert(tool_call_id.clone(), tool_name.to_string());
        }
        self.unseen_tool_results.insert(tool_call_id.clone());
        self.add_message(json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }));
    }

    /// Mark every buffered tool result as seen by the model.
    ///
    /// Call immediately after a successful provider response to the request
    /// built from this manager state, before adding that response's new
    /// assistant/tool messages. At that point every tool result already in
    /// the request has been consumed successfully. Never call on network
    /// errors, timeouts, stream disconnects, or cancellations: those results
    /// must remain inline so the next retry keeps its first observation.
    pub(crate) fn mark_sent_tool_results_seen(&mut self) {
        self.unseen_tool_results.clear();
    }

    /// Logical tool name for the `tool` message at `index`: the name recorded
    /// at insertion (the logical `mcp__*` call for bridge turns), falling back
    /// to the history-derived label for legacy messages.
    fn logical_tool_name(&self, index: usize) -> Option<String> {
        let message = self.buffer.get(index)?;
        if message_role(message)? != "tool" {
            return None;
        }
        let call_id = message.get("tool_call_id").and_then(Value::as_str)?;
        if let Some(name) = self.tool_names.get(call_id) {
            return Some(name.clone());
        }
        tool_call_target(&self.buffer, index).map(|(name, _)| name)
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
    /// Recoverable offloading is tried first: the exact model-visible result
    /// moves into the conversation-owned Observation Store with a compact
    /// `obs-*` stub left behind. When the store cannot retain a candidate
    /// (capacity, `observation_read` results, tiny results), the historical
    /// non-recoverable stub is kept. Unseen results are never touched.
    ///
    /// Returns the number of characters reclaimed.
    /// Structured compaction report: reclaimed bytes plus how many results
    /// became recoverable observations, fell back, or were protected.
    /// Back-compat shim: reclaimed bytes only. Production uses
    /// [`Self::compact_buffer_report`] for the structured counts.
    #[allow(dead_code)]
    pub(crate) fn compact_buffer(&mut self) -> usize {
        self.compact_buffer_report().reclaimed_bytes
    }

    pub(crate) fn compact_buffer_report(&mut self) -> CompactionReport {
        let before = self.buffer_chars;
        let mut report = CompactionReport::default();

        for index in self.superseded_tool_indices() {
            match self.offload_tool_result(index, ObservationReason::Superseded) {
                OffloadOutcome::Recovered { original_bytes } => {
                    report.recoverable_offloads += 1;
                    report.recoverable_original_bytes += original_bytes;
                }
                OffloadOutcome::SkippedUnseen => {
                    report.skipped_unseen += 1;
                }
                OffloadOutcome::SkippedSmall
                | OffloadOutcome::SkippedAlready
                | OffloadOutcome::SkippedNonTool => {}
                OffloadOutcome::NeedsFallback => {
                    // The stub is not free. Replacing a two-byte "ok" with a
                    // sentence naming the call makes the buffer *larger*.
                    if message_serialized_len(&self.buffer[index]) <= MIN_ELIDABLE_TOOL_CHARS {
                        continue;
                    }
                    let label = tool_call_label(&self.buffer, index)
                        .unwrap_or_else(|| "identical call".to_string());
                    replace_tool_content(
                        &mut self.buffer[index],
                        &format!("(superseded by a later {label}; its newer result is below)"),
                    );
                    report.fallback_elisions += 1;
                }
            }
        }

        // Everything before the last few exchanges is history the model has
        // already folded into what it did next.
        let keep_from = retain_boundary(&self.buffer, RECENT_BUFFER_MESSAGES_KEPT);
        for index in 0..keep_from {
            if message_role(&self.buffer[index]) != Some("tool") {
                continue;
            }
            match self.offload_tool_result(index, ObservationReason::Historical) {
                OffloadOutcome::Recovered { original_bytes } => {
                    report.recoverable_offloads += 1;
                    report.recoverable_original_bytes += original_bytes;
                }
                OffloadOutcome::SkippedUnseen => {
                    report.skipped_unseen += 1;
                }
                OffloadOutcome::SkippedSmall
                | OffloadOutcome::SkippedAlready
                | OffloadOutcome::SkippedNonTool => {}
                OffloadOutcome::NeedsFallback => {
                    let size = message_serialized_len(&self.buffer[index]);
                    if size <= MIN_ELIDABLE_TOOL_CHARS {
                        continue;
                    }
                    let label = tool_call_label(&self.buffer, index)
                        .unwrap_or_else(|| "tool result".to_string());
                    replace_tool_content(
                        &mut self.buffer[index],
                        &format!("(elided: {label}, {size} bytes; call it again if you need it)"),
                    );
                    report.fallback_elisions += 1;
                }
            }
        }

        self.buffer_chars = sum_message_lengths(&self.buffer);
        let reclaimed = before.saturating_sub(self.buffer_chars);
        report.reclaimed_bytes = reclaimed;
        // The measured prompt size describes the larger request that
        // `should_summarize` just fired on. Left in place, a
        // `prompt_tokens`-triggered summary is billed again even when the
        // free pass already shrank the buffer enough to fit: the next
        // request is smaller, but `last_prompt_tokens` still names the old
        // one. Scale only the buffer-attributable portion down, keeping any
        // overhead (system prompt, tool schemas, summary) intact. When the
        // estimate is wrong the next measured response corrects it, while
        // always summarizing wastes a paid request every turn.
        if reclaimed > 0 && self.last_prompt_tokens > 0 && before > 0 {
            const CHARS_PER_TOKEN: usize = 4;
            let before_tokens = (before / CHARS_PER_TOKEN) as u64;
            let new_tokens = (self.buffer_chars / CHARS_PER_TOKEN) as u64;
            let overhead = self.last_prompt_tokens.saturating_sub(before_tokens);
            self.last_prompt_tokens = overhead.saturating_add(new_tokens);
        }
        report
    }

    /// Try to offload one `tool` result into the Observation Store.
    ///
    /// Verifies the message shape, refuses unseen results (the model has not
    /// consumed them once yet) and already-offloaded stubs, resolves the
    /// logical tool name, enforces the minimum-useful-size rule, attempts
    /// bounded store insertion, and replaces the content with the
    /// deterministic reference stub. Returns the exact reclaimed serialized
    /// bytes on success.
    fn offload_tool_result(&mut self, index: usize, reason: ObservationReason) -> OffloadOutcome {
        let message = match self.buffer.get(index) {
            Some(message) => message,
            None => return OffloadOutcome::SkippedNonTool,
        };
        if message_role(message) != Some("tool") {
            return OffloadOutcome::SkippedNonTool;
        }
        let tool_call_id = match message.get("tool_call_id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => return OffloadOutcome::SkippedNonTool,
        };
        if self.unseen_tool_results.contains(&tool_call_id) {
            return OffloadOutcome::SkippedUnseen;
        }
        // Only plain-string tool content is offloadable. Array-shaped content
        // is left inline: rewriting it as a string would change the provider
        // message shape, and provider compatibility outranks compaction here.
        let content = match message.get("content").and_then(Value::as_str) {
            Some(content) => content.to_string(),
            None => return OffloadOutcome::SkippedNonTool,
        };
        if is_observation_stub(&content) {
            return OffloadOutcome::SkippedAlready;
        }
        let tool_name = self
            .logical_tool_name(index)
            .unwrap_or_else(|| "tool result".to_string());
        // `observation_read` output itself must never create another
        // observation pointing at the same content; the original `obs-*`
        // remains the retrieval authority.
        if is_observation_read_tool(&tool_name) {
            return OffloadOutcome::NeedsFallback;
        }
        // Cheap early check: replacing a tiny result with a stub grows the
        // prompt. The exact byte comparison below is authoritative.
        if message_serialized_len(&self.buffer[index]) <= MIN_ELIDABLE_TOOL_CHARS {
            return OffloadOutcome::SkippedSmall;
        }
        let original_message_json_bytes = message_serialized_len(&self.buffer[index]);
        let original_content_bytes = content.len();

        // A tool-call id is unique per call, so an existing observation for
        // the same id with identical content is the only safe reuse: the
        // stored bytes and the current bytes agree, and no new entry is
        // needed. Differing content means history and store diverged; fall
        // back rather than pointing the stub at stale bytes or recording a
        // second entry for one id.
        if let Some(existing_id) = self.observation_id_for_tool_call(&tool_call_id) {
            let matches = self
                .observations
                .get(&existing_id)
                .is_some_and(|entry| entry.content == content);
            if !matches {
                return OffloadOutcome::NeedsFallback;
            }
            let stub = observation_stub(&tool_name, original_content_bytes, &existing_id, reason);
            {
                let mut prospective = self.buffer[index].clone();
                replace_tool_content(&mut prospective, &stub);
                if message_serialized_len(&prospective) >= original_message_json_bytes {
                    return OffloadOutcome::SkippedSmall;
                }
            }
            replace_tool_content(&mut self.buffer[index], &stub);
            return OffloadOutcome::Recovered {
                original_bytes: original_content_bytes,
            };
        }
        // Prospective stub length without consuming an id on failure: the id
        // format is fixed-width `obs-{:06}`, so the next id's length is known
        // before allocation. The insert happens only after this guard passes,
        // so a rejected candidate never consumes store capacity.
        let prospective_id = format!("obs-{:06}", self.observations.next_id_for_peek());
        let prospective_stub =
            observation_stub(&tool_name, original_content_bytes, &prospective_id, reason);
        {
            let mut prospective = self.buffer[index].clone();
            replace_tool_content(&mut prospective, &prospective_stub);
            if message_serialized_len(&prospective) >= original_message_json_bytes {
                return OffloadOutcome::SkippedSmall;
            }
        }
        let obs_id = match self.observations.insert(
            &tool_call_id,
            &tool_name,
            content,
            original_message_json_bytes,
        ) {
            Some(id) => id,
            None => return OffloadOutcome::NeedsFallback,
        };
        debug_assert_eq!(
            obs_id, prospective_id,
            "fixed-width observation ids must match the prospective id"
        );
        let stub = observation_stub(&tool_name, original_content_bytes, &obs_id, reason);
        replace_tool_content(&mut self.buffer[index], &stub);
        OffloadOutcome::Recovered {
            original_bytes: original_content_bytes,
        }
    }

    fn observation_id_for_tool_call(&self, tool_call_id: &str) -> Option<String> {
        self.observations
            .entries_for_tool_call(tool_call_id)
            .into_iter()
            .next()
    }

    /// Observation footprint for diagnostics: lifetime storage plus active
    /// references currently in the provider-bound conversation. Only active
    /// references contribute current prompt saving.
    pub(crate) fn observation_footprint(&self) -> ObservationFootprint {
        let stored_entries = self.observations.len();
        let stored_content_bytes = self.observations.stored_content_bytes();
        let mut active_references = 0usize;
        let mut active_original_message_bytes = 0usize;
        let mut active_stub_message_bytes = 0usize;
        for (id, stub_bytes) in active_observation_references(&self.buffer) {
            if let Some(entry) = self.observations.get(&id) {
                active_references += 1;
                active_original_message_bytes += entry.original_message_json_bytes;
                active_stub_message_bytes += stub_bytes;
            }
        }
        let active_reclaimed_json_bytes =
            active_original_message_bytes.saturating_sub(active_stub_message_bytes);
        ObservationFootprint {
            stored_entries,
            stored_content_bytes,
            active_references,
            active_original_message_bytes,
            active_stub_message_bytes,
            active_reclaimed_json_bytes,
        }
    }
}

/// Payload for the `pre-compact` hook: existing compaction fields plus
/// observation attribution. Built here so the chat loop stays lean.
pub(in super::super) fn precompact_hook_payload(
    manager: &ConversationManager,
    reason: &str,
    buffer_before: usize,
    report: &CompactionReport,
    will_summarize: bool,
) -> Value {
    json!({
        "reason": reason,
        "buffer_chars": manager.buffer_size_chars(),
        "buffer_chars_before": buffer_before,
        "buffer_messages": manager.buffer_len(),
        "reclaimed_chars": report.reclaimed_bytes,
        "observation_offloads": report.recoverable_offloads,
        "observation_original_bytes": report.recoverable_original_bytes,
        "fallback_elisions": report.fallback_elisions,
        "skipped_unseen": report.skipped_unseen,
        "last_prompt_tokens": manager.last_prompt_tokens(),
        "prompt_token_budget": manager.prompt_token_budget(),
        "will_summarize": will_summarize,
    })
}
