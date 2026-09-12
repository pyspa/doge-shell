use anyhow::{Error, Result, anyhow};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tracing::debug;

use crate::config::{self, OpenAiConfig};
use crate::stream::{DeltaAggregator, SseFrameSplitter, is_done_marker};
use crate::usage;

/// Budget for establishing the connection, separate from the total budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const CANCELLED_MESSAGE: &str = "OpenAI request cancelled by Ctrl+C";

/// Attempts after the first one for transient failures (429, 5xx, timeouts).
const MAX_RETRIES: usize = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);

/// Optional body fields that an OpenAI-compatible endpoint may reject outright.
/// When a 400 names one of these we drop it and retry once, so a local server
/// that only speaks the older schema still works.
///
/// Order matters: `unsupported_field` returns the first match, and
/// `"stream_options"` contains `"stream"` as a substring, so the more
/// specific name must come first. A 400 naming only `"stream"` still falls
/// through to it correctly, since `"stream_options"` will not match text
/// that never mentions it.
const DROPPABLE_FIELDS: &[&str] = &[
    "max_completion_tokens",
    "response_format",
    "prompt_cache_key",
    // A fixed-temperature model that this build does not recognise still
    // rejects the field. Dropping it costs one retry instead of the whole turn.
    "temperature",
    "stream_options",
    "stream",
    // A compatible server that does not implement `reasoning_effort` at all
    // (as opposed to one that just rejects it alongside `tools`, handled by
    // `reasoning_effort_conflict` below) still needs an escape hatch.
    "reasoning_effort",
];

#[derive(Debug)]
struct RequestCancelled;

impl fmt::Display for RequestCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CANCELLED_MESSAGE)
    }
}

impl std::error::Error for RequestCancelled {}

/// A non-success HTTP response, or a response body carrying an `error` object.
#[derive(Debug)]
pub struct ApiError {
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "OpenAI API error (HTTP {status}): {}", self.message),
            None => write!(f, "OpenAI API error: {}", self.message),
        }
    }
}

/// Failure from one streaming attempt.
///
/// Once at least one delta has reached the caller the reply is already
/// partially on the screen, so `send_streaming_with_retry` only retries the
/// `BeforeFirstDelta` case - retrying past that point would show the start
/// of the answer twice.
#[derive(Debug)]
enum StreamError {
    BeforeFirstDelta(Error),
    AfterFirstDelta(Error),
}

impl std::error::Error for ApiError {}

/// Returns true when the provided error represents a Ctrl+C cancellation
/// triggered during an OpenAI request.
pub fn is_ctrl_c_cancelled(err: &Error) -> bool {
    err.downcast_ref::<RequestCancelled>().is_some()
}

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
    /// JSON object. See [`ChatGptClient::send_chat_streaming`].
    pub stream: bool,
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

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }
}

/// One runtime for every blocking OpenAI call.
///
/// Building a runtime per request also threw away reqwest's connection pool,
/// so every call paid for a fresh TLS handshake.
static SHARED_RUNTIME: LazyLock<Option<tokio::runtime::Runtime>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .ok()
});

fn shared_runtime() -> Result<&'static tokio::runtime::Runtime> {
    SHARED_RUNTIME
        .as_ref()
        .ok_or_else(|| anyhow!("failed to start the OpenAI client runtime"))
}

#[derive(Debug, Clone)]
pub struct ChatGptClient {
    api_key: String,
    default_model: String,
    chat_endpoint: String,
    client: Client,
    /// The operator's configured request budget (`AI_CHAT_TIMEOUT_SECS`).
    ///
    /// A streaming request repurposes this as the maximum silence between
    /// chunks rather than a cap on the whole reply - a streamed answer can
    /// legitimately run longer than one non-streaming turn's budget. See
    /// `send_chat_streaming`.
    request_timeout: Duration,
    /// Optional body fields this endpoint rejected with a 400.
    ///
    /// Remembered so a server that only speaks the older schema is probed once
    /// instead of paying a failed round-trip on every request.
    unsupported: Arc<Mutex<Vec<&'static str>>>,
    /// The operator's configured `reasoning_effort` (`AI_CHAT_REASONING_EFFORT`),
    /// sent on every request that supports the field.
    default_reasoning_effort: Option<String>,
    /// Set once a `tools` request came back naming a `reasoning_effort` /
    /// `tools` conflict, so every later `tools` request on this client sends
    /// `reasoning_effort: "none"` instead of paying that round-trip again.
    ///
    /// Kept out of `unsupported`: that vector means "never send this field",
    /// which is the opposite of what forcing `"none"` needs `build_body` to do.
    /// `Arc` because `ChatGptClient` is `Clone` and callers hand clones around
    /// (e.g. `dsh/src/repl/mod.rs`) - the learning must follow all of them.
    force_reasoning_none: Arc<AtomicBool>,
}

impl ChatGptClient {
    pub fn new(api_key: String) -> Result<Self> {
        Self::new_with_settings(api_key, None, None)
    }

    pub fn new_with_model(api_key: String, model: Option<String>) -> Result<Self> {
        Self::new_with_settings(api_key, model, None)
    }

    pub fn new_with_settings(
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
    ) -> Result<Self> {
        let config = OpenAiConfig::new(Some(api_key), base_url, model);
        Self::try_from_config(&config)
    }

    pub fn try_from_config(config: &OpenAiConfig) -> Result<Self> {
        let api_key = config.api_key().ok_or_else(|| {
            anyhow!(
                "OpenAI-compatible API key is not configured. {}",
                crate::API_KEY_SETUP_HINT
            )
        })?;

        let client = Self {
            api_key: api_key.to_string(),
            default_model: config.default_model().to_string(),
            chat_endpoint: config.chat_endpoint(),
            client: Self::build_client(config.timeout())?,
            request_timeout: config.timeout(),
            unsupported: Arc::new(Mutex::new(Vec::new())),
            default_reasoning_effort: config.reasoning_effort().map(str::to_string),
            force_reasoning_none: Arc::new(AtomicBool::new(false)),
        };
        Ok(client)
    }

    // `send_message`, `send_message_with_model` and the positional
    // `send_chat_request` used to live here. Nothing outside this file called
    // any of them, and `send_message_with_model` held the last
    // `choices[0]["message"]["content"]` in the repository - the exact read
    // `turn::answer_text` exists to replace, kept alive as an example to copy.
    // `send_chat` / `send_chat_streaming` plus `ChatRequestOptions` is the
    // whole surface.

    /// Send a chat completion request, retrying transient failures.
    pub fn send_chat(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<Value> {
        let body = self.build_body(messages, options);
        self.block_on(self.send_with_retry(body, cancel_check))?
    }

    /// Send a chat completion request with the reply delivered incrementally.
    ///
    /// `on_delta` is called with each piece of assistant text as it arrives.
    /// A partial tool call cannot be run, so this still returns only once the
    /// turn completes - the same aggregated `Value` shape [`Self::send_chat`]
    /// returns, built by [`crate::stream::DeltaAggregator`]. Everything past
    /// this call (`turn::interpret_response`, history compaction, usage
    /// accounting) reads it exactly as it would a non-streaming response and
    /// does not need to change.
    ///
    /// Falls back to a single non-streaming response when the endpoint
    /// ignores `options.stream` (a `Content-Type` other than
    /// `text/event-stream`) or rejects it outright - folded into the same
    /// `DROPPABLE_FIELDS` mechanism `send_chat` uses for other optional
    /// fields, so a server that does not understand `stream` costs one
    /// retry instead of failing the turn.
    pub fn send_chat_streaming(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        let body = self.build_body(messages, options);
        self.block_on(self.send_streaming_with_retry(body, cancel_check, on_delta))?
    }

    fn known_unsupported(&self) -> Vec<&'static str> {
        self.unsupported
            .lock()
            .map(|fields| fields.clone())
            .unwrap_or_default()
    }

    fn remember_unsupported(&self, field: &'static str) {
        if let Ok(mut fields) = self.unsupported.lock()
            && !fields.contains(&field)
        {
            fields.push(field);
        }
    }

    fn reasoning_none_forced(&self) -> bool {
        // SeqCst, not Relaxed: this flag changes what a concurrent `build_body`
        // call on a cloned client (e.g. the command-palette `LiveAiService`
        // shares a clone with the suggestion backend, `dsh/src/repl/mod.rs`)
        // puts on the wire, not just a debug print, so a store should become
        // visible to another thread's load promptly. This does not make the
        // read-then-maybe-write sequence atomic: two concurrent requests can
        // still both read `false` before either stores `true`, sending one
        // request without the correction it needed. That race is harmless -
        // the losing request just hits the same 400 and self-heals on its
        // own next attempt - not a data race to prevent at all costs.
        self.force_reasoning_none.load(Ordering::SeqCst)
    }

    /// Remember that `tools` requests on this client must force
    /// `reasoning_effort: "none"`, and warn once per process per model - a new
    /// `ChatGptClient` is built for every `!` message
    /// (`dsh-builtin/src/chatgpt.rs`), so a plain "once ever" flag would warn
    /// about the first model hit and then silently skip every other model the
    /// operator later switches to.
    fn remember_reasoning_none_forced(&self, model: &str) {
        static WARNED_MODELS: LazyLock<Mutex<HashSet<String>>> =
            LazyLock::new(|| Mutex::new(HashSet::new()));

        if !self.force_reasoning_none.swap(true, Ordering::SeqCst) {
            debug!(
                chat_direction = "retry",
                reason = "reasoning_effort conflicts with tools",
                model = model
            );
        }

        let first_time_for_this_model = WARNED_MODELS
            .lock()
            .map(|mut warned| warned.insert(model.to_string()))
            .unwrap_or(true);
        if !first_time_for_this_model {
            return;
        }

        // Deliberately says "its current" rather than "its default" reasoning
        // effort: `build_body` skips the proactive model-gated default
        // whenever the operator set `AI_CHAT_REASONING_EFFORT` explicitly, so
        // this path also fires when *that* value is what conflicted with
        // `tools` - not the model's own default - and blaming the model then
        // would send an operator checking model docs instead of their own env.
        eprintln!(
            "dsh: model `{model}` does not allow function tools together with its current \
             reasoning effort setting, so `!` chat resent this turn with reasoning_effort: none. \
             Set AI_CHAT_REASONING_EFFORT=none to skip the extra round-trip, or pick a \
             different model if tool-using answers need full reasoning."
        );
    }

    fn block_on<F: Future>(&self, future: F) -> Result<F::Output> {
        let runtime = shared_runtime()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            // Avoid a nested-runtime panic by handing the current worker over to
            // blocking work instead of calling Handle::block_on().
            Ok(tokio::task::block_in_place(|| runtime.block_on(future)))
        } else {
            Ok(runtime.block_on(future))
        }
    }

    async fn send_with_retry(
        &self,
        mut body: Value,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<Value> {
        let mut attempt = 0usize;
        let mut recovery = RecoveryState::seed(self.known_unsupported());

        loop {
            match self.send_once(&body, cancel_check).await {
                Ok(data) => return Ok(data),
                Err(err) => {
                    if is_ctrl_c_cancelled(&err) {
                        return Err(err);
                    }

                    if self.recover(&err, &mut body, &mut recovery) {
                        continue;
                    }

                    attempt += 1;
                    let Some(delay) = retry_delay(&err, attempt) else {
                        return Err(err);
                    };

                    debug!(
                        chat_direction = "retry",
                        attempt = attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %err
                    );
                    sleep_with_cancel(delay, cancel_check).await?;
                }
            }
        }
    }

    async fn send_once(
        &self,
        body: &Value,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<Value> {
        let builder = self
            .client
            .post(&self.chat_endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(body);

        let response = Self::await_with_cancel(builder.send(), cancel_check).await?;
        let status = response.status();
        let retry_after = parse_retry_after(response.headers());
        let text = Self::await_with_cancel(response.text(), cancel_check).await?;

        if !status.is_success() {
            return Err(ApiError {
                status: Some(status.as_u16()),
                retry_after,
                message: error_message_from_body(&text, status),
            }
            .into());
        }

        let data: Value = serde_json::from_str(&text)
            .map_err(|err| anyhow!("failed to parse the OpenAI response: {err}"))?;

        if let Some(message) = error_message_from_value(&data) {
            return Err(ApiError {
                status: None,
                retry_after,
                message,
            }
            .into());
        }

        usage::record_response(&data);

        let choices_len = data
            .get("choices")
            .and_then(|choices| choices.as_array())
            .map(|choices| choices.len())
            .unwrap_or(0);
        debug!(
            chat_direction = "response",
            choices = choices_len,
            usage = ?usage::TokenUsage::from_response(&data)
        );

        Ok(data)
    }

    async fn send_streaming_with_retry(
        &self,
        mut body: Value,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        let mut attempt = 0usize;
        let mut recovery = RecoveryState::seed(self.known_unsupported());

        loop {
            match self
                .send_once_streaming(&body, cancel_check, on_delta)
                .await
            {
                Ok(data) => return Ok(data),
                // Some of the reply is already on the screen: retrying would
                // show its start twice, so this is the end of the line.
                Err(StreamError::AfterFirstDelta(err)) => return Err(err),
                Err(StreamError::BeforeFirstDelta(err)) => {
                    if is_ctrl_c_cancelled(&err) {
                        return Err(err);
                    }

                    // Dropping `stream` itself (one of the recoverable fields)
                    // degrades this turn to a single non-streaming response
                    // rather than failing it.
                    if self.recover(&err, &mut body, &mut recovery) {
                        continue;
                    }

                    attempt += 1;
                    let Some(delay) = retry_delay(&err, attempt) else {
                        return Err(err);
                    };

                    debug!(
                        chat_direction = "retry",
                        attempt = attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %err
                    );
                    sleep_with_cancel(delay, cancel_check).await?;
                }
            }
        }
    }

    async fn send_once_streaming(
        &self,
        body: &Value,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value, StreamError> {
        let builder = self
            .client
            .post(&self.chat_endpoint)
            // The client's default timeout is a non-streaming turn's budget;
            // a stream can legitimately run longer. `consume_event_stream`
            // enforces its own no-data timeout instead.
            .timeout(Duration::from_secs(config::MAX_TIMEOUT_SECS))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(body);

        let response = Self::await_with_cancel(builder.send(), cancel_check)
            .await
            .map_err(StreamError::BeforeFirstDelta)?;

        let status = response.status();
        let retry_after = parse_retry_after(response.headers());
        let is_event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));

        if !status.is_success() {
            let text = Self::await_with_cancel(response.text(), cancel_check)
                .await
                .map_err(StreamError::BeforeFirstDelta)?;
            return Err(StreamError::BeforeFirstDelta(
                ApiError {
                    status: Some(status.as_u16()),
                    retry_after,
                    message: error_message_from_body(&text, status),
                }
                .into(),
            ));
        }

        if !is_event_stream {
            // The endpoint accepted the request but ignored `stream: true`
            // and answered in one JSON object, as some OpenAI-compatible
            // servers do. Read it the same way `send_once` would.
            let text = Self::await_with_cancel(response.text(), cancel_check)
                .await
                .map_err(StreamError::BeforeFirstDelta)?;
            let data: Value = serde_json::from_str(&text).map_err(|err| {
                StreamError::BeforeFirstDelta(anyhow!("failed to parse the OpenAI response: {err}"))
            })?;
            if let Some(message) = error_message_from_value(&data) {
                return Err(StreamError::BeforeFirstDelta(
                    ApiError {
                        status: None,
                        retry_after,
                        message,
                    }
                    .into(),
                ));
            }
            usage::record_response(&data);
            return Ok(data);
        }

        self.consume_event_stream(response, cancel_check, on_delta)
            .await
    }

    /// Read one SSE response to completion, aggregating it into the same
    /// shape `send_once` would return.
    async fn consume_event_stream(
        &self,
        response: reqwest::Response,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value, StreamError> {
        use futures::StreamExt;

        let mut byte_stream = response.bytes_stream();
        let mut splitter = SseFrameSplitter::new();
        let mut aggregator = DeltaAggregator::new();
        let mut delta_sent = false;

        // Held for the whole stream, not recreated per chunk: a signal
        // handler is only meaningful to register once.
        let ctrl_c_future = async {
            if cancel_check.is_some() {
                std::future::pending::<bool>().await
            } else {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => true,
                    Err(e) => {
                        debug!("dsh-openai: Failed to listen for Ctrl+C via tokio: {}", e);
                        std::future::pending::<bool>().await
                    }
                }
            }
        };
        tokio::pin!(ctrl_c_future);

        let mut poll_interval = tokio::time::interval(Duration::from_millis(50));
        poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Freshly armed each iteration: the budget is silence *between*
            // chunks, not a cap on the whole reply.
            let idle_deadline = tokio::time::sleep(self.request_timeout);
            tokio::pin!(idle_deadline);
            let next_item = byte_stream.next();
            tokio::pin!(next_item);

            let poll_result = loop {
                tokio::select! {
                    item = &mut next_item => break Ok(item),
                    true = &mut ctrl_c_future => break Err(RequestCancelled.into()),
                    _ = &mut idle_deadline => break Err(anyhow!(
                        "no data received from the OpenAI stream for {:?}",
                        self.request_timeout
                    )),
                    _ = poll_interval.tick() => {
                        if let Some(check) = cancel_check
                            && check() {
                                break Err(RequestCancelled.into());
                            }
                    }
                }
            };

            let item = match poll_result {
                Ok(item) => item,
                Err(err) => return Err(Self::wrap_stream_error(err, delta_sent)),
            };

            let chunk = match item {
                Some(Ok(chunk)) => chunk,
                Some(Err(err)) => return Err(Self::wrap_stream_error(err.into(), delta_sent)),
                None => {
                    // The connection closed without a trailing blank line
                    // after the last event - flush whatever is pending.
                    for payload in splitter.finish() {
                        Self::apply_stream_payload(
                            &payload,
                            &mut aggregator,
                            &mut delta_sent,
                            on_delta,
                        )?;
                    }
                    // No `[DONE]` and no `finish_reason` means the
                    // connection dropped mid-turn, not that the model
                    // finished. Without this check the partial content
                    // collected so far would be returned as a normal,
                    // complete `Answer` - a truncated reply with no sign
                    // anything went wrong.
                    if !aggregator.has_finish_reason() {
                        return Err(Self::wrap_stream_error(
                            anyhow!(
                                "the OpenAI stream closed before the reply finished \
                                 (no finish_reason received)"
                            ),
                            delta_sent,
                        ));
                    }
                    let value = aggregator.finish();
                    usage::record_response(&value);
                    return Ok(value);
                }
            };

            for payload in splitter.push(&chunk) {
                if is_done_marker(&payload) {
                    let value = aggregator.finish();
                    usage::record_response(&value);
                    return Ok(value);
                }
                Self::apply_stream_payload(&payload, &mut aggregator, &mut delta_sent, on_delta)?;
            }
        }
    }

    fn wrap_stream_error(err: Error, delta_sent: bool) -> StreamError {
        if delta_sent {
            StreamError::AfterFirstDelta(err)
        } else {
            StreamError::BeforeFirstDelta(err)
        }
    }

    /// Decode one SSE frame payload and, if it carried a text delta, forward
    /// it to the caller.
    ///
    /// A single malformed frame is not fatal to the turn - it is dropped
    /// with a debug log, the way a client tolerates one corrupted keep-alive
    /// rather than losing an otherwise complete answer.
    fn apply_stream_payload(
        payload: &str,
        aggregator: &mut DeltaAggregator,
        delta_sent: &mut bool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<(), StreamError> {
        let chunk: Value = match serde_json::from_str(payload) {
            Ok(chunk) => chunk,
            Err(err) => {
                debug!(
                    chat_direction = "response",
                    reason = "malformed SSE frame",
                    error = %err
                );
                return Ok(());
            }
        };

        // A provider can report a mid-stream failure as its own frame
        // (`data: {"error": {...}}`) instead of a non-2xx status - the same
        // shape `error_message_from_value` already handles for a plain JSON
        // response. This frame has no `choices` array, so without this
        // check `DeltaAggregator::apply`'s early return would treat it as
        // an empty heartbeat, and the caller would see a stall or a
        // truncated answer instead of the real failure.
        if let Some(message) = error_message_from_value(&chunk) {
            let err = ApiError {
                status: None,
                retry_after: None,
                message,
            };
            return Err(Self::wrap_stream_error(err.into(), *delta_sent));
        }

        if let Some(text) = aggregator.apply(&chunk) {
            *delta_sent = true;
            on_delta(&text);
        }
        Ok(())
    }

    async fn await_with_cancel<F, T, E>(
        future: F,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<T>
    where
        F: Future<Output = Result<T, E>>,
        anyhow::Error: From<E>,
    {
        tokio::pin!(future);

        // Attempt to listen for Ctrl+C only if we don't have an external check
        // If an external check is provided, we assume the caller handles signals and updates the check state.
        let ctrl_c_future = async {
            if cancel_check.is_some() {
                std::future::pending::<bool>().await
            } else {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => true,
                    Err(e) => {
                        debug!("dsh-openai: Failed to listen for Ctrl+C via tokio: {}", e);
                        std::future::pending::<bool>().await
                    }
                }
            }
        };
        tokio::pin!(ctrl_c_future);

        // Check for cancellation more frequently for better responsiveness
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        // Ensure the first tick completes immediately so we don't wait 50ms unnecessarily
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                res = &mut future => return res.map_err(anyhow::Error::from),
                // If tokio's ctrl_c fires, treat it as a cancellation
                true = &mut ctrl_c_future => return Err(RequestCancelled.into()),
                _ = interval.tick() => {
                    if let Some(check) = cancel_check
                        && check() {
                            return Err(RequestCancelled.into());
                        }
                }
            }
        }
    }

    /// Try to fix up `body` in response to a 400 the request just failed with,
    /// so the caller can retry instead of spending one of the transient-error
    /// attempts on it. Returns whether `body` changed.
    ///
    /// Checked in this order:
    /// 1. `reasoning_effort_conflict` - additive (forces `"none"` into the
    ///    body) - must run first. If `unsupported_field` ran first it could
    ///    match `"reasoning_effort"` too (the message names the field either
    ///    way) and *drop* it instead; the server's non-`none` default would
    ///    stay in effect and the identical 400 would come back with nothing
    ///    left in `state` to try.
    /// 2. `unsupported_field` - an optional field the endpoint rejects outright.
    fn recover(&self, err: &Error, body: &mut Value, state: &mut RecoveryState) -> bool {
        if let Some(model) = reasoning_effort_conflict(err, body, &state.dropped) {
            if let Some(map) = body.as_object_mut() {
                map.insert("reasoning_effort".into(), json!("none"));
            }
            self.remember_reasoning_none_forced(&model);
            return true;
        }

        if let Some(field) = unsupported_field(err, body, &state.dropped) {
            debug!(
                chat_direction = "retry",
                reason = "unsupported field",
                field = field
            );
            // `remember_unsupported` is a persistent, client-wide memo, but
            // this client can carry more than one model over its life
            // (`AI_SUMMARY_MODEL`, the session-scoped client shared between
            // ghost text and the command palette). For every other droppable
            // field that memo is harmless: the field is purely optional and
            // dropping it for a model that never asked to receive it changes
            // nothing. `reasoning_effort` is different - it is sometimes the
            // *fix* for a `tools` conflict, not just an optional extra - so
            // letting an unrelated, `tools`-less model's rejection disable it
            // client-wide would permanently block `reasoning_effort_conflict`
            // (its `already_dropped` guard) from correcting the model that
            // actually needs `tools` to work, for the rest of this client's
            // life. Only a `tools`-bearing request - the one the field
            // protects - earns the persistent memo for `reasoning_effort`;
            // a `tools`-less request still gets the field dropped for this
            // one retry (`state.dropped`) without poisoning later requests.
            let has_tools = body.get("tools").is_some();
            if let Some(map) = body.as_object_mut() {
                map.remove(field);
            }
            state.dropped.push(field);
            if field != "reasoning_effort" || has_tools {
                self.remember_unsupported(field);
            }
            return true;
        }

        false
    }

    fn build_client(total_timeout: Duration) -> Result<Client> {
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

    fn build_body(&self, messages: &[Value], options: &ChatRequestOptions) -> Value {
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

/// Model families in OpenAI's current reasoning lineup (GPT-5.x and the
/// o-series). Matching on a prefix rather than one exact id is what keeps a
/// new point release - `gpt-5.1`, `gpt-5.6-luna`, `o3-mini` - from missing a
/// fixup this whole family needs the moment it becomes the configured model.
///
/// Two independent constraints happen to apply to the whole family today:
/// it samples at a fixed temperature and 400s on any other value
/// (`build_body`'s `final_temperature`), and it 400s a `tools` request unless
/// `reasoning_effort` is `"none"` (`build_body`'s `reasoning_effort`; see
/// `reasoning_effort_conflict` for the self-healing correction that still
/// covers a model not in this list, or a family member whose newer point
/// release drops the restriction). Both read [`is_openai_reasoning_model`],
/// so a future model picking up one constraint without the other needs a
/// second prefix list, not just a second function.
const OPENAI_REASONING_MODEL_PREFIXES: &[&str] = &["gpt-5", "o1", "o3", "o4"];

/// Whether `model` is a known member of [`OPENAI_REASONING_MODEL_PREFIXES`].
///
/// `pub` so `doctor` (`dsh-builtin`) can show its own auto-`"none"` caveat
/// only for a model this actually applies to, instead of re-deriving the
/// prefix list as a second, driftable copy.
pub fn is_openai_reasoning_model(model: &str) -> bool {
    // A provider route (`openai/gpt-5-mini`) names the same model. Lowercase
    // first: `AI_CHAT_MODEL=GPT-5-Mini` is the same model as `gpt-5-mini`,
    // and a case-sensitive miss here would silently fall back to the slower,
    // reactive 400-then-correct path for a plausible casing choice.
    let model = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    OPENAI_REASONING_MODEL_PREFIXES
        .iter()
        .any(|prefix| family_matches(&model, prefix))
}

/// Whether `model` is `prefix` or a variant of it.
///
/// Only `-` and `.` end a family name, so `gpt-5.1-codex` and `o3-mini` match
/// `gpt-5` and `o3` while `gpt-51` and `o1x-turbo` - different models that
/// merely share a leading substring - do not.
fn family_matches(model: &str, prefix: &str) -> bool {
    let Some(rest) = model.strip_prefix(prefix) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('-') || rest.starts_with('.')
}

async fn sleep_with_cancel(delay: Duration, cancel_check: Option<&dyn Fn() -> bool>) -> Result<()> {
    let sleep = async move {
        tokio::time::sleep(delay).await;
        Ok::<(), std::convert::Infallible>(())
    };
    ChatGptClient::await_with_cancel(sleep, cancel_check).await
}

/// Backoff for a failure worth retrying, or `None` when it is terminal.
fn retry_delay(err: &Error, attempt: usize) -> Option<Duration> {
    if attempt > MAX_RETRIES {
        return None;
    }

    let backoff = RETRY_BASE_DELAY
        .saturating_mul(1u32 << (attempt.clamp(1, 8) as u32 - 1))
        .min(MAX_RETRY_DELAY);

    if let Some(api_error) = err.downcast_ref::<ApiError>() {
        let status = api_error.status?;
        let retryable = status == StatusCode::TOO_MANY_REQUESTS.as_u16()
            || status == StatusCode::REQUEST_TIMEOUT.as_u16()
            || (500..600).contains(&status);
        if !retryable {
            return None;
        }
        return Some(
            api_error
                .retry_after
                .map(|after| after.min(MAX_RETRY_DELAY).max(backoff))
                .unwrap_or(backoff),
        );
    }

    // Connect failures fail fast (bounded by CONNECT_TIMEOUT), so retrying them
    // is cheap. A timeout already burned the full request budget: retrying it
    // would multiply the worst case by MAX_RETRIES and freeze the shell.
    if let Some(request_error) = err.downcast_ref::<reqwest::Error>()
        && request_error.is_connect()
    {
        return Some(backoff);
    }

    None
}

/// One request's accumulated 400 recoveries: optional fields dropped so far.
/// Seeded from the client's cross-request memory (`known_unsupported`) so a
/// server already known to reject something is not probed again.
///
/// A `reasoning_effort` force needs no equivalent flag here: `build_body`
/// already mirrors the client's `reasoning_none_forced()` into the body
/// before the first attempt, and `recover` mutates that same body in place on
/// every retry, so the body's own `reasoning_effort` value is always
/// authoritative - `reasoning_effort_conflict`'s `already_none` check reads
/// it directly instead of tracking a parallel bool that could drift from it.
struct RecoveryState {
    dropped: Vec<&'static str>,
}

impl RecoveryState {
    fn seed(dropped: Vec<&'static str>) -> Self {
        Self { dropped }
    }
}

/// The lowercased message of a 400 `ApiError`, or `None` for anything else
/// (a different status, or an error `downcast_ref` can't identify as one).
/// Shared by `reasoning_effort_conflict` and `unsupported_field` so the two
/// 400-sniffing checks can't drift on how a bad request is recognised.
fn bad_request_message(err: &Error) -> Option<String> {
    let api_error = err.downcast_ref::<ApiError>()?;
    (api_error.status == Some(StatusCode::BAD_REQUEST.as_u16()))
        .then(|| api_error.message.to_ascii_lowercase())
}

/// Whether `haystack` contains `word` as a whole word - split on anything
/// that is not alphanumeric or `_` - rather than as a raw substring.
///
/// Guards `reasoning_effort_conflict` against a coincidental embedded match
/// ("dysfunctional" contains "function"; "toolchain" contains "tool") being
/// misread as a `tools` conflict. It does not catch every false positive - a
/// gateway that echoes the request body verbatim, or a domain name like
/// "tool.example.com", still splits into a bare "tool" token - but a
/// misdiagnosis here costs at most one wasted round-trip before
/// `reasoning_effort_conflict`'s own `already_dropped` / `already_none`
/// guards stop it from repeating, so perfect precision is not worth chasing.
fn contains_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|token| token == word)
}

/// Whether `err` is a 400 naming a `reasoning_effort` / `tools` conflict that
/// forcing `reasoning_effort: "none"` into the body can fix, and if so the
/// model that reported it (for the one-time warning).
///
/// Requires the message to mention both `reasoning_effort` and a whole word
/// naming tools or function calling (`contains_word`) - not the exact
/// phrase, so a gateway wording the same conflict differently (e.g.
/// "function calling" instead of "tools") still matches - but not
/// `reasoning_effort` alone. An operator typo in `AI_CHAT_REASONING_EFFORT`
/// produces a 400 that names the field too (e.g. "Invalid value for
/// reasoning_effort: must be one of ..."); without the second word that
/// error would be misread as a `tools` conflict, forced to `"none"` and
/// latched for the rest of the client's life over what was actually a
/// config typo.
fn reasoning_effort_conflict(
    err: &Error,
    body: &Value,
    already_dropped: &[&'static str],
) -> Option<String> {
    if already_dropped.contains(&"reasoning_effort") {
        return None;
    }

    let message = bad_request_message(err)?;
    // Real messages say "tools" (plural) and "function", not the bare
    // singular "tool" - check every inflection actually seen, not just one.
    let mentions_tools_or_function = ["tool", "tools", "function", "functions"]
        .iter()
        .any(|word| contains_word(&message, word));
    if !message.contains("reasoning_effort") || !mentions_tools_or_function {
        return None;
    }

    // `build_body` only inserts `tools` when non-empty, so presence alone
    // means this request actually offers function calling.
    body.get("tools")?;

    let already_none = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "none");
    if already_none {
        return None;
    }

    Some(
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or("configured model")
            .to_string(),
    )
}

/// Name the optional body field a 400 complained about, if any.
fn unsupported_field(
    err: &Error,
    body: &Value,
    already_dropped: &[&'static str],
) -> Option<&'static str> {
    let message = bad_request_message(err)?;
    DROPPABLE_FIELDS.iter().copied().find(|field| {
        !already_dropped.contains(field) && message.contains(*field) && body.get(*field).is_some()
    })
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|secs| *secs >= 0.0)
        .map(Duration::from_secs_f64)
}

fn error_message_from_value(data: &Value) -> Option<String> {
    let error = data.get("error")?;
    if error.is_null() {
        return None;
    }
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    Some(truncate_for_display(&message))
}

fn error_message_from_body(text: &str, status: StatusCode) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text)
        && let Some(message) = error_message_from_value(&value)
    {
        return message;
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_string();
    }
    truncate_for_display(trimmed)
}

fn truncate_for_display(text: &str) -> String {
    const MAX: usize = 400;
    if text.len() <= MAX {
        return text.to_string();
    }
    let end = text.floor_char_boundary(MAX);
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests;
