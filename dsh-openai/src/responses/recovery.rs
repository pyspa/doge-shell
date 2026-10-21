//! API-key Responses 400 recovery: an optional field the endpoint rejects is
//! dropped once and remembered for that endpoint and model.
//!
//! The chat/completions path does the same with `DROPPABLE_FIELDS` in
//! `client.rs`; Responses names fields differently (`reasoning.effort`,
//! `max_output_tokens`, `include`), so the list is separate. Subscription
//! never uses this: SIWC accepts exactly the body `wire` builds for it.
use super::*;
use std::sync::{LazyLock, Mutex};

/// Optional top-level body fields `wire::build_body` may add on the API-key
/// route. Everything else (`model`, `input`, `tools`, `text`) is required for
/// the request to mean what the caller asked.
const DROPPABLE: &[&str] = &[
    "temperature",
    "reasoning",
    "max_output_tokens",
    "prompt_cache_key",
    "include",
];

type Key = (String, String);
static REJECTED: LazyLock<Mutex<HashMap<Key, Vec<&'static str>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remove the fields this endpoint already rejected for `model`. Remembered
/// per process, not per client: `!` chat builds a new client every message,
/// and a per-client memory would pay the same 400 on each one.
pub(super) fn strip_known(endpoint: &str, model: &str, body: &mut Value) {
    let key = (endpoint.to_owned(), model.to_owned());
    let known = REJECTED
        .lock()
        .ok()
        .and_then(|rejected| rejected.get(&key).cloned())
        .unwrap_or_default();
    if let Some(map) = body.as_object_mut() {
        for field in known {
            map.remove(field);
        }
    }
}

/// When `error` is a 400 naming an optional field still in `body`, drop it,
/// remember it, and report which one so the caller retries.
pub(super) fn drop_rejected(
    endpoint: &str,
    model: &str,
    error: &anyhow::Error,
    body: &mut Value,
) -> Option<&'static str> {
    let message = crate::client::bad_request_message(error)?;
    let field = DROPPABLE.iter().copied().find(|field| {
        body.get(*field).is_some()
            && (crate::client::contains_word(&message, field)
                || (*field == "include" && message.contains("encrypted")))
    })?;
    body.as_object_mut()?.remove(field);
    if let Ok(mut rejected) = REJECTED.lock() {
        let fields = rejected
            .entry((endpoint.to_owned(), model.to_owned()))
            .or_default();
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    tracing::debug!(
        chat_direction = "retry",
        reason = "responses endpoint rejected an optional field",
        field,
        model
    );
    Some(field)
}
