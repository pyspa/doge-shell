//! Token accounting for OpenAI-compatible chat completions.
//!
//! The API already reports usage on every response; recording it here is what
//! makes the cost of an agent turn observable at all. Without it there is no
//! way to tell whether a context change helped or hurt.

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// Token counts for one or more chat completion responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    /// Prompt tokens the provider served from its prefix cache.
    pub cached_prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Requests whose input/output counts were unavailable.
    #[serde(default)]
    pub unknown_usage_requests: u64,
    /// Requests whose cached-input count was unavailable.
    #[serde(default)]
    pub unknown_cached_requests: u64,
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
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| usage.get("cached_tokens").and_then(Value::as_u64));
        let cached_prompt_tokens = cached.unwrap_or(0);

        if usage.get("prompt_tokens").and_then(Value::as_u64).is_none()
            && usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .is_none()
        {
            return None;
        }

        Some(Self {
            requests: 0,
            prompt_tokens,
            cached_prompt_tokens,
            completion_tokens,
            unknown_usage_requests: 0,
            unknown_cached_requests: u64::from(cached.is_none()),
        })
    }

    /// Count one response against this tally.
    ///
    /// Callers that need a figure for their own turn must accumulate locally:
    /// the process-wide totals also move when a background request finishes.
    pub fn add_response(&mut self, response: &Value) {
        // Saturating throughout: the counts come from the server, and a
        // wrapped total would reset the figure a turn budget is checked
        // against.
        self.requests = self.requests.saturating_add(1);
        if let Some(usage) = Self::from_response(response) {
            self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
            self.cached_prompt_tokens = self
                .cached_prompt_tokens
                .saturating_add(usage.cached_prompt_tokens);
            self.completion_tokens = self
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            self.unknown_cached_requests = self
                .unknown_cached_requests
                .saturating_add(usage.unknown_cached_requests);
        } else {
            self.unknown_usage_requests = self.unknown_usage_requests.saturating_add(1);
            self.unknown_cached_requests = self.unknown_cached_requests.saturating_add(1);
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }

    /// Prompt tokens the provider did not serve from its prefix cache.
    ///
    /// Saturating: a broken or odd provider response reporting more cached
    /// tokens than prompt tokens must not underflow.
    pub fn uncached_prompt_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_sub(self.cached_prompt_tokens)
    }

    /// Share of prompt tokens served from the provider's prefix cache.
    ///
    /// `None` when the request reported no prompt tokens at all; callers
    /// must render that as "unavailable", never as `0%`.
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        (self.prompt_tokens > 0 && self.unknown_cached_requests == 0)
            .then(|| self.cached_prompt_tokens as f64 / self.prompt_tokens as f64)
    }

    pub fn is_empty(&self) -> bool {
        self.requests == 0 && self.total_tokens() == 0
    }

    /// Usage accumulated between an earlier snapshot and this one.
    pub fn since(&self, earlier: &Self) -> Self {
        Self {
            unknown_usage_requests: self
                .unknown_usage_requests
                .saturating_sub(earlier.unknown_usage_requests),
            unknown_cached_requests: self
                .unknown_cached_requests
                .saturating_sub(earlier.unknown_cached_requests),
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
        let cached = if self.unknown_cached_requests > 0 {
            "unknown".into()
        } else {
            self.cached_prompt_tokens.to_string()
        };
        let suffix = if self.unknown_usage_requests > 0 {
            format!(" ({} req usage unknown)", self.unknown_usage_requests)
        } else {
            String::new()
        };
        format!(
            "{} req / in {} (cached {}) / out {}{}",
            self.requests, self.prompt_tokens, cached, self.completion_tokens, suffix
        )
    }
}

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static PROMPT_TOKENS: AtomicU64 = AtomicU64::new(0);
static CACHED_PROMPT_TOKENS: AtomicU64 = AtomicU64::new(0);
static COMPLETION_TOKENS: AtomicU64 = AtomicU64::new(0);
static UNKNOWN_USAGE: AtomicU64 = AtomicU64::new(0);
static UNKNOWN_CACHED: AtomicU64 = AtomicU64::new(0);

/// `fetch_add` that sticks at `u64::MAX` instead of wrapping to a small total.
fn saturating_fetch_add(counter: &AtomicU64, value: u64) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        match counter.compare_exchange_weak(
            current,
            current.saturating_add(value),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// Add `usage` to the process-wide totals.
pub fn record(usage: TokenUsage) {
    saturating_fetch_add(&UNKNOWN_USAGE, usage.unknown_usage_requests);
    saturating_fetch_add(&UNKNOWN_CACHED, usage.unknown_cached_requests);
    saturating_fetch_add(&REQUESTS, usage.requests);
    saturating_fetch_add(&PROMPT_TOKENS, usage.prompt_tokens);
    saturating_fetch_add(&CACHED_PROMPT_TOKENS, usage.cached_prompt_tokens);
    saturating_fetch_add(&COMPLETION_TOKENS, usage.completion_tokens);
}

/// Count one completed request and any usage it reported.
pub(crate) fn record_response(response: &Value) {
    let mut usage = TokenUsage::from_response(response).unwrap_or(TokenUsage {
        unknown_usage_requests: 1,
        unknown_cached_requests: 1,
        ..Default::default()
    });
    usage.requests = 1;
    record(usage);
}

/// Totals since the shell started.
pub fn session_total() -> TokenUsage {
    TokenUsage {
        unknown_usage_requests: UNKNOWN_USAGE.load(Ordering::Relaxed),
        unknown_cached_requests: UNKNOWN_CACHED.load(Ordering::Relaxed),
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
            ..Default::default()
        };
        let later = TokenUsage {
            requests: 5,
            prompt_tokens: 350,
            cached_prompt_tokens: 240,
            completion_tokens: 35,
            ..Default::default()
        };

        let delta = later.since(&earlier);
        assert_eq!(delta.requests, 3);
        assert_eq!(delta.prompt_tokens, 250);
        assert_eq!(delta.cached_prompt_tokens, 200);
        assert_eq!(delta.completion_tokens, 25);
        assert_eq!(delta.summary_line(), "3 req / in 250 (cached 200) / out 25");
    }

    /// A hostile or broken server can report any u64. Summing it with `+`
    /// panicked in debug and wrapped in release, which reset the tally a
    /// turn budget is checked against.
    #[test]
    fn huge_server_token_counts_saturate_instead_of_overflowing() {
        let huge = json!({
            "usage": {
                "prompt_tokens": u64::MAX,
                "completion_tokens": u64::MAX,
                "prompt_tokens_details": { "cached_tokens": u64::MAX }
            }
        });
        let mut total = TokenUsage::default();
        total.add_response(&huge);
        total.add_response(&huge);
        assert_eq!(total.prompt_tokens, u64::MAX);
        assert_eq!(total.cached_prompt_tokens, u64::MAX);
        assert_eq!(total.completion_tokens, u64::MAX);
        assert_eq!(total.total_tokens(), u64::MAX);

        let mut counts = TokenUsage {
            requests: u64::MAX,
            unknown_usage_requests: u64::MAX,
            unknown_cached_requests: u64::MAX,
            ..Default::default()
        };
        counts.add_response(&json!({}));
        assert_eq!(counts.requests, u64::MAX);
        assert_eq!(counts.unknown_usage_requests, u64::MAX);
        assert_eq!(counts.unknown_cached_requests, u64::MAX);
    }

    /// The process-wide totals saturate the same way (tested on a local
    /// atomic: the real statics are shared with every other test).
    #[test]
    fn the_process_wide_counters_saturate() {
        let counter = AtomicU64::new(u64::MAX - 1);
        saturating_fetch_add(&counter, 5);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
        saturating_fetch_add(&counter, u64::MAX);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);

        let counter = AtomicU64::new(3);
        saturating_fetch_add(&counter, 4);
        assert_eq!(counter.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn session_total_accumulates() {
        let before = session_total();
        record(TokenUsage {
            requests: 1,
            prompt_tokens: 7,
            cached_prompt_tokens: 3,
            completion_tokens: 2,
            ..Default::default()
        });
        let after = session_total();

        let delta = after.since(&before);
        assert_eq!(delta.requests, 1);
        assert_eq!(delta.prompt_tokens, 7);
        assert_eq!(delta.cached_prompt_tokens, 3);
        assert_eq!(delta.completion_tokens, 2);
    }

    #[test]
    fn cache_math_reports_uncached_tokens_and_hit_ratio() {
        let usage = TokenUsage {
            requests: 1,
            prompt_tokens: 1000,
            cached_prompt_tokens: 750,
            completion_tokens: 50,
            ..Default::default()
        };

        assert_eq!(usage.uncached_prompt_tokens(), 250);
        assert_eq!(usage.cache_hit_ratio(), Some(0.75));
        // Cached tokens are still input tokens: the total is unchanged.
        assert_eq!(usage.total_tokens(), 1050);
    }

    #[test]
    fn cache_math_never_underflows_on_an_odd_provider_response() {
        let usage = TokenUsage {
            requests: 1,
            prompt_tokens: 100,
            cached_prompt_tokens: 250,
            completion_tokens: 0,
            ..Default::default()
        };

        assert_eq!(usage.uncached_prompt_tokens(), 0);
    }

    #[test]
    fn cache_math_reports_no_ratio_without_prompt_tokens() {
        let usage = TokenUsage::default();

        assert_eq!(usage.cache_hit_ratio(), None);
    }
    #[test]
    fn missing_usage_and_cached_counts_remain_unknown() {
        let mut tally = TokenUsage::default();
        tally.add_response(&serde_json::json!({"choices":[]}));
        assert_eq!(tally.unknown_usage_requests, 1);
        assert!(tally.summary_line().contains("usage unknown"));
        let known_input = TokenUsage::from_response(
            &serde_json::json!({"usage":{"prompt_tokens":12,"completion_tokens":2}}),
        )
        .unwrap();
        assert_eq!(known_input.unknown_cached_requests, 1);
        assert_eq!(known_input.cache_hit_ratio(), None);
        assert!(known_input.summary_line().contains("cached unknown"));
        let zero = TokenUsage::from_response(&serde_json::json!({"usage":{"prompt_tokens":0,"completion_tokens":0,"cached_tokens":0}})).unwrap();
        assert_eq!(zero.unknown_usage_requests, 0);
    }
}
