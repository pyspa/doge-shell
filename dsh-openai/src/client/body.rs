//! Building the outgoing request: the HTTP client itself (`build_client`) and the JSON body (`build_body`), including the per-model `reasoning_effort` default and any fields this client has
//! already learned the endpoint rejects.
use super::*;

impl ChatGptClient {
    pub(super) fn build_client(total_timeout: Duration) -> Result<Client> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(total_timeout)
            .build()?;
        Ok(client)
    }

    /// The `reasoning_effort` value `build_body` should send, or `None` to
    /// omit the field. `has_tools` reflects the *outgoing* body (after
    /// `options.tools` has been filtered for emptiness), not `options.tools`
    /// directly.
    ///
    /// Forces `"none"` when either:
    /// - this client already learned the endpoint rejects the configured
    ///   value over `reasoning_effort` (a previous 400 asked for it), or
    /// - the operator set no preference at all and `model` is a known member
    ///   of the reasoning lineup that needs it up front - avoiding that 400
    ///   instead of paying and then correcting it once per `!` message
    ///   (`is_openai_reasoning_model`).
    ///
    /// An explicit `AI_CHAT_REASONING_EFFORT` is tried as configured first
    /// even on that lineup, so `recover`'s correction still applies if it
    /// turns out to conflict. Without `tools` this always returns the
    /// configured value (or `None`) untouched, so a request that never uses
    /// function calling (summarization, `safe_run`'s JSON generation) keeps
    /// the configured - or model's own default - reasoning quality.
    fn resolve_reasoning_effort(&self, has_tools: bool, model: &str) -> Option<&str> {
        let forces_none = has_tools
            && (self.reasoning_none_forced()
                || (self.default_reasoning_effort.is_none() && is_openai_reasoning_model(model)));
        if forces_none {
            Some("none")
        } else {
            self.default_reasoning_effort.as_deref()
        }
    }

    pub(super) fn build_body(&self, messages: &[Value], options: &ChatRequestOptions) -> Value {
        let selected_model = options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        let final_temperature = if is_openai_reasoning_model(&selected_model) {
            Some(1.0)
        } else {
            options.temperature
        };

        let mut body = json!({
            "model": selected_model,
            "messages": messages,
        });

        let unsupported = self.known_unsupported();
        let supported = |field: &'static str| !unsupported.contains(&field);

        let map = body
            .as_object_mut()
            .expect("chat request body is a JSON object");

        if let Some(temperature) = final_temperature
            && supported("temperature")
        {
            map.insert("temperature".into(), json!(temperature));
        }
        if let Some(tools) = &options.tools
            && !tools.is_empty()
        {
            map.insert("tools".into(), json!(tools));
        }
        let has_tools = map.contains_key("tools");
        let reasoning_effort = self.resolve_reasoning_effort(has_tools, &selected_model);
        if let Some(reasoning_effort) = reasoning_effort
            && supported("reasoning_effort")
        {
            map.insert("reasoning_effort".into(), json!(reasoning_effort));
        }
        if let Some(max_tokens) = options.max_tokens
            && supported("max_completion_tokens")
        {
            map.insert("max_completion_tokens".into(), json!(max_tokens));
        }
        if let Some(response_format) = &options.response_format
            && supported("response_format")
        {
            map.insert("response_format".into(), response_format.clone());
        }
        if let Some(key) = &options.prompt_cache_key
            && supported("prompt_cache_key")
        {
            map.insert("prompt_cache_key".into(), json!(key));
        }
        if options.stream && supported("stream") {
            map.insert("stream".into(), json!(true));
            // Without this the provider omits `usage` from the final chunk,
            // which starves the turn-budget and summarization-trigger
            // accounting that reads it (`chatgpt.rs::should_summarize`).
            if supported("stream_options") {
                map.insert("stream_options".into(), json!({ "include_usage": true }));
            }
        }

        debug!(
            chat_direction = "request",
            model = %selected_model,
            message_count = messages.len(),
            tool_count = options.tools.as_ref().map(|t| t.len()).unwrap_or(0),
            temperature = ?final_temperature,
            max_tokens = ?options.max_tokens,
            stream = options.stream,
            reasoning_effort = ?reasoning_effort
        );

        body
    }
}
