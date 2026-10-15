use serde_json::Value;

/// Per-request knobs. Kept separate from the legacy positional helpers so that
/// adding a field does not ripple through every call site.
#[derive(Debug, Clone, Default)]
pub struct ChatRequestOptions {
    pub temperature: Option<f64>,
    pub model: Option<String>,
    pub tools: Option<Vec<Value>>,
    /// Cap on generated tokens, sent as `max_completion_tokens`.
    ///
    /// Beware: on a reasoning model (the `gpt-5` family, o-series) this budget
    /// also covers hidden reasoning tokens, so a tight cap comes back as
    /// `finish_reason: "length"` with no content at all. Leave it unset unless
    /// the endpoint is known not to reason.
    pub max_tokens: Option<u64>,
    /// Structured-output request, e.g. `{"type": "json_object"}`.
    pub response_format: Option<Value>,
    /// Cache-routing hint for providers that support it.
    pub prompt_cache_key: Option<String>,
    /// Ask the provider to deliver the reply as SSE chunks instead of one
    /// JSON object. See [`crate::ChatGptClient::send_chat_streaming`].
    pub stream: bool,
    /// Conversation identity for OpenCode headers only; never included in JSON.
    pub session_id: Option<String>,
}

impl ChatRequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    pub fn with_tools(mut self, tools: Option<Vec<Value>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: Option<u64>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_response_format(mut self, response_format: Option<Value>) -> Self {
        self.response_format = response_format;
        self
    }

    pub fn with_prompt_cache_key(mut self, key: Option<String>) -> Self {
        self.prompt_cache_key = key;
        self
    }

    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }
}
