//! Conversation-owned Observation Store: recoverable offloading for tool results.
//!
//! Large historical tool results that [`super::conversation::ConversationManager`]
//! removes from active LLM context are preserved here as exact model-visible
//! strings, with a compact reference stub left in the buffer. The model can
//! recover an offloaded result through `observation_read` without re-running
//! the original tool. The store is bounded, conversation-scoped, and
//! serialized with the conversation (session file / task checkpoint).
pub(crate) mod compaction;
#[cfg(test)]
mod tests;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum number of observations retained per conversation.
pub(crate) const MAX_OBSERVATION_ENTRIES: usize = 256;
/// Maximum total model-visible content bytes retained per conversation.
///
/// Derived from the tool-output limits: a normal tool result is at most
/// `MAX_TOOL_OUTPUT_CHARS` (8192), paged tools cap near 6 KiB, and only
/// `tool_describe` may approach 96 KiB. 1 MiB therefore holds well over a
/// hundred typical results while bounding session-file and checkpoint growth.
pub(crate) const MAX_OBSERVATION_STORE_BYTES: usize = 1024 * 1024;
/// Maximum model-visible content bytes for a single observation.
///
/// Covers the largest single tool result (`tool_describe` up to 96 KiB plus
/// hook notes and `[task event N]`), with headroom. Larger candidates fall
/// back to the ordinary non-recoverable stub.
pub(crate) const MAX_SINGLE_OBSERVATION_BYTES: usize = 128 * 1024;

/// Prefix for every observation stub left in the buffer.
pub(crate) const OBSERVATION_STUB_PREFIX: &str = "(offloaded tool result:";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ObservationStore {
    next_id: u64,
    entries: BTreeMap<String, Observation>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Observation {
    pub(crate) id: String,
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    /// Exact final `content` string previously shown to the model.
    pub(crate) content: String,
    pub(crate) original_content_bytes: usize,
    pub(crate) original_message_json_bytes: usize,
}

/// Why a tool result is being offloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservationReason {
    Superseded,
    Historical,
}

impl ObservationStore {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn stored_content_bytes(&self) -> usize {
        self.entries.values().map(|entry| entry.content.len()).sum()
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Observation> {
        self.entries.get(id)
    }

    pub(crate) fn entries_for_tool_call(&self, tool_call_id: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.tool_call_id == tool_call_id)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(crate) fn next_id_for_peek(&self) -> u64 {
        self.next_id + 1
    }

    fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("obs-{:06}", self.next_id)
    }

    /// Attempt to retain `content`. Returns the new observation id, or `None`
    /// when limits forbid retention (caller falls back to ordinary elision).
    /// Never evicts an existing entry: when capacity is reached, new
    /// recoverable observations stop rather than invalidating older ids.
    pub(crate) fn insert(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        content: String,
        original_message_json_bytes: usize,
    ) -> Option<String> {
        if content.len() > MAX_SINGLE_OBSERVATION_BYTES {
            return None;
        }
        if self.entries.len() >= MAX_OBSERVATION_ENTRIES {
            return None;
        }
        if self.stored_content_bytes().saturating_add(content.len()) > MAX_OBSERVATION_STORE_BYTES {
            return None;
        }
        let id = self.allocate_id();
        let original_content_bytes = content.len();
        self.entries.insert(
            id.clone(),
            Observation {
                id: id.clone(),
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                content,
                original_content_bytes,
                original_message_json_bytes,
            },
        );
        Some(id)
    }

    /// Bounded UTF-8-safe window over one observation.
    ///
    /// `offset` is a byte offset; `limit` is a byte limit. Both are clamped to
    /// character boundaries (`start` rounds up, `end` rounds down) so a split
    /// code point is never returned. Sequential reads with `offset = end`
    /// reassemble losslessly. Progress is guaranteed: a non-empty remainder
    /// always yields a non-empty window (at least one full codepoint), so a
    /// small `limit` over multi-byte text cannot stall paging with an empty
    /// page at the same offset.
    pub(crate) fn read_window(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, usize, usize, String), String> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| format!("unknown observation `{id}`"))?;
        let total = entry.content.len();
        let mut start = offset.min(total);
        while start < total && !entry.content.is_char_boundary(start) {
            start += 1;
        }
        let mut end = (start.saturating_add(limit)).min(total);
        while end > start && !entry.content.is_char_boundary(end) {
            end -= 1;
        }
        if end == start && start < total {
            end = entry
                .content
                .ceil_char_boundary(start.saturating_add(1).min(total));
        }
        Ok((start, end, total, entry.content[start..end].to_string()))
    }
}

/// Format the compact deterministic stub left where a result used to be.
///
/// Short, stable, and paid every request: names the logical tool, the
/// original size, and the observation id, and tells the model how to
/// recover. Never copies arguments, paths, or result snippets.
pub(crate) fn observation_stub(
    tool_name: &str,
    original_content_bytes: usize,
    id: &str,
    reason: ObservationReason,
) -> String {
    match reason {
        ObservationReason::Superseded => format!(
            "(offloaded tool result: {tool_name}, {original_content_bytes} B, observation {id}; superseded by a later call, newer result below; use observation_read if needed)"
        ),
        ObservationReason::Historical => format!(
            "(offloaded tool result: {tool_name}, {original_content_bytes} B, observation {id}; use observation_read if needed)"
        ),
    }
}

/// Extract the observation id from a stub produced by [`observation_stub`].
/// Returns `None` for ordinary results, superseded/elided stubs, and
/// `observation_read` output headers.
pub(crate) fn parse_observation_stub(content: &str) -> Option<String> {
    if !content.starts_with(OBSERVATION_STUB_PREFIX) {
        return None;
    }
    let marker = "observation obs-";
    let position = content.find(marker)?;
    let rest = &content[position + "observation ".len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(rest.len());
    let id = &rest[..end];
    if id.starts_with("obs-") && !id["obs-".len()..].is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

/// Whether `content` is already an observation reference (must not be
/// offloaded again).
pub(crate) fn is_observation_stub(content: &str) -> bool {
    parse_observation_stub(content).is_some()
}

/// Whether `tool_name` names the retrieval tool itself. Its results are
/// excluded from recoverable offloading: offloading them would chain
/// observation-of-observation entries while the original `obs-*` remains the
/// retrieval authority.
pub(crate) fn is_observation_read_tool(tool_name: &str) -> bool {
    tool_name == super::tool::observation::NAME
}

/// Active observation references currently in the buffer: `(observation id,
/// current stub message bytes)` pairs.
pub(crate) fn active_observation_references(buffer: &[serde_json::Value]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for message in buffer {
        if message.get("role").and_then(|v| v.as_str()) != Some("tool") {
            continue;
        }
        if let Some(content) = message.get("content").and_then(|v| v.as_str())
            && let Some(id) = parse_observation_stub(content)
        {
            out.push((id, message.to_string().len()));
        }
    }
    out
}

/// Tool-call ids still present in the buffer, for pruning sidecar state after
/// prefix drops and rewinds.
pub(crate) fn buffer_tool_call_ids(buffer: &[serde_json::Value]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for message in buffer {
        if let Some(id) = message.get("tool_call_id").and_then(|v| v.as_str()) {
            ids.insert(id.to_string());
        }
        if let Some(calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for call in calls {
                if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids
}
