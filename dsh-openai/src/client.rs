use anyhow::{Error, Result, anyhow};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tracing::debug;

use crate::config::OpenAiConfig;
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
const DROPPABLE_FIELDS: &[&str] = &[
    "max_completion_tokens",
    "response_format",
    "prompt_cache_key",
    // A fixed-temperature model that this build does not recognise still
    // rejects the field. Dropping it costs one retry instead of the whole turn.
    "temperature",
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
    /// Optional body fields this endpoint rejected with a 400.
    ///
    /// Remembered so a server that only speaks the older schema is probed once
    /// instead of paying a failed round-trip on every request.
    unsupported: Arc<Mutex<Vec<&'static str>>>,
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
        let api_key = config
            .api_key()
            .ok_or_else(|| anyhow!("OpenAI-compatible API key is not configured"))?;

        let client = Self {
            api_key: api_key.to_string(),
            default_model: config.default_model().to_string(),
            chat_endpoint: config.chat_endpoint(),
            client: Self::build_client(config.timeout())?,
            unsupported: Arc::new(Mutex::new(Vec::new())),
        };
        Ok(client)
    }

    pub fn send_message(
        &self,
        input: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<String> {
        self.send_message_with_model(input, prompt, temperature, None, cancel_check)
    }

    pub fn send_message_with_model(
        &self,
        input: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        model: Option<String>,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<String> {
        let messages = Self::build_messages(input, prompt);
        let options = ChatRequestOptions::new()
            .with_temperature(temperature)
            .with_model(model);
        let data = self.send_chat(&messages, &options, cancel_check)?;

        let output = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("Unexpected response {data}"))?;

        Ok(output.to_string())
    }

    pub fn send_chat_request(
        &self,
        messages: &[Value],
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<&[Value]>,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<Value> {
        let options = ChatRequestOptions::new()
            .with_temperature(temperature)
            .with_model(model)
            .with_tools(tools.map(|items| items.to_vec()));
        self.send_chat(messages, &options, cancel_check)
    }

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
        let mut dropped_fields: Vec<&'static str> = self.known_unsupported();

        loop {
            match self.send_once(&body, cancel_check).await {
                Ok(data) => return Ok(data),
                Err(err) => {
                    if is_ctrl_c_cancelled(&err) {
                        return Err(err);
                    }

                    // An endpoint that rejects an optional field: drop it once
                    // and try again rather than failing the whole turn.
                    if let Some(field) = unsupported_field(&err, &body, &dropped_fields) {
                        debug!(
                            chat_direction = "retry",
                            reason = "unsupported field",
                            field = field
                        );
                        if let Some(map) = body.as_object_mut() {
                            map.remove(field);
                        }
                        dropped_fields.push(field);
                        self.remember_unsupported(field);
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

    fn build_client(total_timeout: Duration) -> Result<Client> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(total_timeout)
            .build()?;
        Ok(client)
    }

    fn build_body(&self, messages: &[Value], options: &ChatRequestOptions) -> Value {
        let selected_model = options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        let final_temperature = if model_requires_default_temperature(&selected_model) {
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

        debug!(
            chat_direction = "request",
            model = %selected_model,
            message_count = messages.len(),
            tool_count = options.tools.as_ref().map(|t| t.len()).unwrap_or(0),
            temperature = ?final_temperature,
            max_tokens = ?options.max_tokens
        );

        body
    }

    fn build_messages(content: &str, prompt: Option<String>) -> Vec<Value> {
        let mut messages = Vec::new();
        if let Some(prompt) = prompt
            && !prompt.trim().is_empty()
        {
            messages.push(json!({ "role": "system", "content": prompt.trim() }));
        }
        messages.push(json!({ "role": "user", "content": content }));
        messages
    }
}

/// Model families that reject any temperature other than the default.
///
/// The reasoning models sample at a fixed temperature and answer a request that
/// sets one with a 400. Matching on a prefix rather than one exact id is what
/// keeps a new point release - `gpt-5.1`, `o3-mini` - from failing every turn
/// the moment it becomes the configured model.
const FIXED_TEMPERATURE_MODEL_PREFIXES: &[&str] = &["gpt-5", "o1", "o3", "o4"];

fn model_requires_default_temperature(model: &str) -> bool {
    // A provider route (`openai/gpt-5-mini`) names the same model.
    let model = model.rsplit('/').next().unwrap_or(model);
    FIXED_TEMPERATURE_MODEL_PREFIXES
        .iter()
        .any(|prefix| family_matches(model, prefix))
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

/// Name the optional body field a 400 complained about, if any.
fn unsupported_field(
    err: &Error,
    body: &Value,
    already_dropped: &[&'static str],
) -> Option<&'static str> {
    let api_error = err.downcast_ref::<ApiError>()?;
    if api_error.status != Some(StatusCode::BAD_REQUEST.as_u16()) {
        return None;
    }

    let message = api_error.message.to_ascii_lowercase();
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
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::{Duration, sleep};

    fn client() -> ChatGptClient {
        ChatGptClient::new_with_settings(
            "test-key".to_string(),
            Some("gpt-5-mini".to_string()),
            Some("https://example.invalid".to_string()),
        )
        .expect("client should initialize")
    }

    #[tokio::test]
    async fn test_await_with_cancel_normal_completion() {
        let future = async { Ok::<_, anyhow::Error>("success") };
        let result = ChatGptClient::await_with_cancel(future, None).await;
        assert_eq!(result.unwrap(), "success");
    }

    #[tokio::test]
    async fn test_await_with_cancel_via_callback() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = cancel_flag.clone();

        // Callback that returns the value of the flag
        let check = move || flag_clone.load(Ordering::SeqCst);

        // Future that waits long enough
        let future = async {
            sleep(Duration::from_secs(5)).await;
            Ok::<_, anyhow::Error>("should not be reached")
        };

        // Spawn a task to set the flag after 200ms
        let flag_clone2 = cancel_flag.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(200)).await;
            flag_clone2.store(true, Ordering::SeqCst);
        });

        let result = ChatGptClient::await_with_cancel(future, Some(&check)).await;

        assert!(result.is_err());
        assert!(is_ctrl_c_cancelled(&result.unwrap_err()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_send_chat_request_inside_runtime_does_not_panic() {
        let client = client();
        let messages = vec![json!({ "role": "user", "content": "hello" })];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.send_chat_request(&messages, Some(0.0), None, None, Some(&|| true))
        }));

        assert!(result.is_ok(), "send_chat_request panicked inside runtime");
        assert!(result.expect("panic check").is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_send_message_inside_runtime_does_not_panic() {
        let client = client();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.send_message("hello", None, Some(0.0), Some(&|| true))
        }));

        assert!(result.is_ok(), "send_message panicked inside runtime");
        assert!(result.expect("panic check").is_err());
    }

    #[test]
    fn build_body_includes_optional_fields() {
        let client = client();
        let messages = vec![json!({ "role": "user", "content": "hi" })];
        let options = ChatRequestOptions::new()
            .with_temperature(Some(0.2))
            .with_model(Some("gpt-4.1-mini".to_string()))
            .with_max_tokens(Some(256))
            .with_response_format(Some(json!({ "type": "json_object" })))
            .with_prompt_cache_key(Some("dsh-agent".to_string()));

        let body = client.build_body(&messages, &options);

        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["max_completion_tokens"], 256);
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["prompt_cache_key"], "dsh-agent");
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_forces_default_temperature_for_gpt_5_mini() {
        let client = client();
        let body = client.build_body(
            &[json!({ "role": "user", "content": "hi" })],
            &ChatRequestOptions::new().with_temperature(Some(0.0)),
        );

        assert_eq!(body["temperature"], 1.0);
    }

    /// The exact-match version of this check let every reasoning model other
    /// than `gpt-5-mini` receive a temperature it answers with a 400.
    #[test]
    fn fixed_temperature_models_are_matched_by_family() {
        for model in [
            "gpt-5",
            "gpt-5-mini",
            "gpt-5.1-codex",
            "o1-preview",
            "o3",
            "o3-mini",
            "o4-mini",
            "openai/gpt-5-mini",
        ] {
            assert!(
                model_requires_default_temperature(model),
                "{model} should use the default temperature"
            );
        }

        for model in ["gpt-4.1-mini", "gpt-4o", "o1x-turbo", "gpt-51", "llama3"] {
            assert!(
                !model_requires_default_temperature(model),
                "{model} should keep the caller's temperature"
            );
        }
    }

    /// An endpoint that rejects `temperature` must cost one retry, not the turn.
    #[test]
    fn build_body_drops_temperature_once_the_endpoint_rejects_it() {
        let client = client();
        client.remember_unsupported("temperature");

        let body = client.build_body(
            &[json!({ "role": "user", "content": "hi" })],
            &ChatRequestOptions::new()
                .with_temperature(Some(0.2))
                .with_model(Some("gpt-4.1-mini".to_string())),
        );

        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn retry_delay_retries_429_and_5xx_but_not_401() {
        let too_many: Error = ApiError {
            status: Some(429),
            retry_after: None,
            message: "slow down".into(),
        }
        .into();
        assert!(retry_delay(&too_many, 1).is_some());

        let server: Error = ApiError {
            status: Some(503),
            retry_after: None,
            message: "unavailable".into(),
        }
        .into();
        assert!(retry_delay(&server, 2).is_some());

        let unauthorized: Error = ApiError {
            status: Some(401),
            retry_after: None,
            message: "bad key".into(),
        }
        .into();
        assert!(retry_delay(&unauthorized, 1).is_none());
    }

    #[test]
    fn retry_delay_gives_up_after_max_retries() {
        let err: Error = ApiError {
            status: Some(500),
            retry_after: None,
            message: "boom".into(),
        }
        .into();

        assert!(retry_delay(&err, MAX_RETRIES).is_some());
        assert!(retry_delay(&err, MAX_RETRIES + 1).is_none());
    }

    #[test]
    fn retry_delay_honours_retry_after() {
        let err: Error = ApiError {
            status: Some(429),
            retry_after: Some(Duration::from_secs(5)),
            message: "slow down".into(),
        }
        .into();

        assert_eq!(retry_delay(&err, 1), Some(Duration::from_secs(5)));
    }

    #[test]
    fn retry_delay_backoff_grows_and_is_capped() {
        let err: Error = ApiError {
            status: Some(500),
            retry_after: None,
            message: "boom".into(),
        }
        .into();

        let first = retry_delay(&err, 1).unwrap();
        let second = retry_delay(&err, 2).unwrap();
        assert!(second > first);
        assert!(retry_delay(&err, MAX_RETRIES).unwrap() <= MAX_RETRY_DELAY);
    }

    #[test]
    fn unsupported_field_is_detected_and_only_dropped_once() {
        let err: Error = ApiError {
            status: Some(400),
            retry_after: None,
            message: "Unsupported parameter: 'max_completion_tokens' is not supported".into(),
        }
        .into();
        let body = json!({ "model": "local", "max_completion_tokens": 32 });

        assert_eq!(
            unsupported_field(&err, &body, &[]),
            Some("max_completion_tokens")
        );
        assert_eq!(
            unsupported_field(&err, &body, &["max_completion_tokens"]),
            None
        );
    }

    #[test]
    fn unsupported_field_ignores_non_400_errors() {
        let err: Error = ApiError {
            status: Some(500),
            retry_after: None,
            message: "max_completion_tokens".into(),
        }
        .into();
        let body = json!({ "max_completion_tokens": 32 });

        assert_eq!(unsupported_field(&err, &body, &[]), None);
    }

    #[test]
    fn retry_delay_does_not_retry_a_timeout() {
        // A timeout already spent the full request budget; retrying multiplies
        // the worst case by MAX_RETRIES.
        let client = Client::builder()
            .timeout(Duration::from_millis(1))
            .build()
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime.block_on(async {
            client
                .get("http://10.255.255.1/")
                .send()
                .await
                .expect_err("must time out")
        });

        assert!(err.is_timeout() || err.is_connect());
        if err.is_timeout() {
            assert!(retry_delay(&anyhow::Error::from(err), 1).is_none());
        }
    }

    #[test]
    fn a_rejected_field_is_remembered_for_later_requests() {
        let client = client();
        let err: Error = ApiError {
            status: Some(400),
            retry_after: None,
            message: "Unsupported parameter: 'response_format'".into(),
        }
        .into();
        let body = json!({ "response_format": { "type": "json_object" } });

        assert_eq!(
            unsupported_field(&err, &body, &client.known_unsupported()),
            Some("response_format")
        );

        client.remember_unsupported("response_format");

        // The next request must not send it again.
        let rebuilt = client.build_body(
            &[json!({ "role": "user", "content": "hi" })],
            &ChatRequestOptions::new().with_response_format(Some(json!({ "type": "json_object" }))),
        );
        assert!(rebuilt.get("response_format").is_none());
        assert_eq!(
            unsupported_field(&err, &body, &client.known_unsupported()),
            None
        );
    }

    #[test]
    fn error_message_prefers_the_api_message() {
        let body =
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
        let message = error_message_from_body(body, StatusCode::UNAUTHORIZED);
        assert_eq!(message, "Incorrect API key provided");
    }

    #[test]
    fn error_message_falls_back_to_status_and_raw_text() {
        assert_eq!(
            error_message_from_body("", StatusCode::BAD_GATEWAY),
            "Bad Gateway"
        );
        assert_eq!(
            error_message_from_body("upstream exploded", StatusCode::BAD_GATEWAY),
            "upstream exploded"
        );
    }

    #[test]
    fn parse_retry_after_reads_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(2)));

        let empty = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&empty), None);
    }
}
