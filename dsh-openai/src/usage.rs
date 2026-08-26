//! Token accounting for OpenAI-compatible chat completions.
//!
//! The API already reports usage on every response; recording it here is what
//! makes the cost of an agent turn observable at all. Without it there is no
//! way to tell whether a context change helped or hurt.

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// Token counts for one or more chat completion responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    /// Prompt tokens the provider served from its prefix cache.
    pub cached_prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl TokenUsage {
    /// Read the `usage` block of a chat completion response.
    ///
    /// Returns `None` when the endpoint omits it, which some OpenAI-compatible
    /// servers do.
    pub fn from_response(response: &Value) -> Option<Self> {
        let usage = response.get("usage")?;

        let prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion_tokens = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cached_prompt_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| usage.get("cached_tokens").and_then(Value::as_u64))
            .unwrap_or(0);

        if prompt_tokens == 0 && completion_tokens == 0 {
            return None;
        }

        Some(Self {
            requests: 0,
            prompt_tokens,
            cached_prompt_tokens,
            completion_tokens,
        })
    }

    /// Count one response against this tally.
    ///
    /// Callers that need a figure for their own turn must accumulate locally:
    /// the process-wide totals also move when a background request finishes.
    pub fn add_response(&mut self, response: &Value) {
        self.requests += 1;
        if let Some(usage) = Self::from_response(response) {
            self.prompt_tokens += usage.prompt_tokens;
            self.cached_prompt_tokens += usage.cached_prompt_tokens;
            self.completion_tokens += usage.completion_tokens;
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    pub fn is_empty(&self) -> bool {
        self.requests == 0 && self.total_tokens() == 0
    }

    /// Usage accumulated between an earlier snapshot and this one.
    pub fn since(&self, earlier: &Self) -> Self {
        Self {
            requests: self.requests.saturating_sub(earlier.requests),
            prompt_tokens: self.prompt_tokens.saturating_sub(earlier.prompt_tokens),
            cached_prompt_tokens: self
                .cached_prompt_tokens
                .saturating_sub(earlier.cached_prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_sub(earlier.completion_tokens),
        }
    }

    /// One-line rendering for the shell and for `doctor ai`.
    pub fn summary_line(&self) -> String {
        format!(
            "{} req / in {} (cached {}) / out {}",
            self.requests, self.prompt_tokens, self.cached_prompt_tokens, self.completion_tokens
        )
    }
}

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static PROMPT_TOKENS: AtomicU64 = AtomicU64::new(0);
static CACHED_PROMPT_TOKENS: AtomicU64 = AtomicU64::new(0);
static COMPLETION_TOKENS: AtomicU64 = AtomicU64::new(0);

/// Add `usage` to the process-wide totals.
pub fn record(usage: TokenUsage) {
    REQUESTS.fetch_add(usage.requests, Ordering::Relaxed);
    PROMPT_TOKENS.fetch_add(usage.prompt_tokens, Ordering::Relaxed);
    CACHED_PROMPT_TOKENS.fetch_add(usage.cached_prompt_tokens, Ordering::Relaxed);
    COMPLETION_TOKENS.fetch_add(usage.completion_tokens, Ordering::Relaxed);
}

/// Count one completed request and any usage it reported.
pub(crate) fn record_response(response: &Value) {
    let mut usage = TokenUsage::from_response(response).unwrap_or_default();
    usage.requests = 1;
    record(usage);
}

/// Totals since the shell started.
pub fn session_total() -> TokenUsage {
    TokenUsage {
        requests: REQUESTS.load(Ordering::Relaxed),
        prompt_tokens: PROMPT_TOKENS.load(Ordering::Relaxed),
        cached_prompt_tokens: CACHED_PROMPT_TOKENS.load(Ordering::Relaxed),
        completion_tokens: COMPLETION_TOKENS.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_usage_with_cached_prompt_details() {
        let response = json!({
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "prompt_tokens_details": { "cached_tokens": 1024 }
            }
        });

        let usage = TokenUsage::from_response(&response).expect("usage");
        assert_eq!(usage.prompt_tokens, 1200);
        assert_eq!(usage.cached_prompt_tokens, 1024);
        assert_eq!(usage.completion_tokens, 80);
        assert_eq!(usage.total_tokens(), 1280);
    }

    #[test]
    fn reads_flat_cached_tokens_field() {
        let response = json!({
            "usage": { "prompt_tokens": 10, "completion_tokens": 1, "cached_tokens": 4 }
        });

        let usage = TokenUsage::from_response(&response).expect("usage");
        assert_eq!(usage.cached_prompt_tokens, 4);
    }

    #[test]
    fn returns_none_without_usage_block() {
        assert!(TokenUsage::from_response(&json!({ "choices": [] })).is_none());
        assert!(TokenUsage::from_response(&json!({ "usage": {} })).is_none());
    }

    #[test]
    fn add_response_accumulates_locally() {
        let mut tally = TokenUsage::default();
        tally.add_response(&json!({
            "usage": { "prompt_tokens": 10, "completion_tokens": 2 }
        }));
        tally.add_response(&json!({ "choices": [] }));

        assert_eq!(tally.requests, 2);
        assert_eq!(tally.prompt_tokens, 10);
        assert_eq!(tally.completion_tokens, 2);
    }

    #[test]
    fn since_reports_the_delta() {
        let earlier = TokenUsage {
            requests: 2,
            prompt_tokens: 100,
            cached_prompt_tokens: 40,
            completion_tokens: 10,
        };
        let later = TokenUsage {
            requests: 5,
            prompt_tokens: 350,
            cached_prompt_tokens: 240,
            completion_tokens: 35,
        };

        let delta = later.since(&earlier);
        assert_eq!(delta.requests, 3);
        assert_eq!(delta.prompt_tokens, 250);
        assert_eq!(delta.cached_prompt_tokens, 200);
        assert_eq!(delta.completion_tokens, 25);
        assert_eq!(delta.summary_line(), "3 req / in 250 (cached 200) / out 25");
    }

    #[test]
    fn session_total_accumulates() {
        let before = session_total();
        record(TokenUsage {
            requests: 1,
            prompt_tokens: 7,
            cached_prompt_tokens: 3,
            completion_tokens: 2,
        });
        let after = session_total();

        let delta = after.since(&before);
        assert_eq!(delta.requests, 1);
        assert_eq!(delta.prompt_tokens, 7);
        assert_eq!(delta.cached_prompt_tokens, 3);
        assert_eq!(delta.completion_tokens, 2);
    }
}
