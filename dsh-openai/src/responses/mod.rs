//! Stateless Responses transport shared by ChatGPT subscription (OAuth) and the
//! `api_key` provider; normalized consumers and native history are distinct.
use crate::{
    ChatRequestOptions,
    auth::{self, Account, AuthStore},
};
use anyhow::{Result, anyhow, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;
mod recovery;
#[cfg(test)]
mod tests;
mod wire;
use wire::{Wire, build_body, normalize};
pub(crate) const CONTINUATION: &str = "_dsh_responses";

/// Who the request is billed to. Subscription keeps its fixed endpoint, OAuth
/// refresh and account-switch cancellation; an API key only adds a Bearer
/// header to the configured endpoint.
#[derive(Clone)]
pub(crate) enum ResponsesAuth {
    Subscription(AuthStore),
    ApiKey {
        key: String,
        /// Truncated SHA-256 of the key: history identity must not hold the key.
        fingerprint: String,
        reasoning_effort: Option<String>,
    },
}

impl fmt::Debug for ResponsesAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subscription(_) => f.write_str("Subscription"),
            Self::ApiKey { .. } => f.write_str("ApiKey"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResponsesTransport {
    pub auth: ResponsesAuth,
    pub model: String,
    pub timeout: Duration,
    client: reqwest::Client,
    endpoint: String,
}
// Dropping a sent request does not prove zero billing. Account for absent terminal usage.
struct AttemptUsage(bool);
impl AttemptUsage {
    fn completed(&mut self, native: &Value) {
        crate::usage::record_response(&json!({"usage":wire::normalized_usage(native)}));
        self.0 = true;
    }
}
impl Drop for AttemptUsage {
    fn drop(&mut self) {
        if !self.0 {
            crate::usage::record_response(&json!({}));
        }
    }
}
impl ResponsesTransport {
    /// ChatGPT subscription transport on the fixed public endpoint.
    pub fn new(store: AuthStore, model: String, timeout: Duration) -> Result<Self> {
        Self::build(
            ResponsesAuth::Subscription(store),
            model,
            timeout,
            "https://api.openai.com/v1/responses".into(),
        )
    }
    /// `api_key` provider transport on the configured `{base_url}/responses`.
    pub fn api_key(
        key: String,
        endpoint: String,
        model: String,
        timeout: Duration,
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        use sha2::{Digest, Sha256};
        let fingerprint = format!("{:x}", Sha256::digest(key.as_bytes()))[..16].to_string();
        Self::build(
            ResponsesAuth::ApiKey {
                key,
                fingerprint,
                reasoning_effort,
            },
            model,
            timeout,
            endpoint,
        )
    }
    fn build(
        auth: ResponsesAuth,
        model: String,
        timeout: Duration,
        endpoint: String,
    ) -> Result<Self> {
        // Subscription never follows a redirect with its OAuth token. The
        // API-key route behaves like the chat/completions client, which
        // follows them (reqwest drops `Authorization` across hosts).
        let redirect = match auth {
            ResponsesAuth::Subscription(_) => reqwest::redirect::Policy::none(),
            ResponsesAuth::ApiKey { .. } => reqwest::redirect::Policy::default(),
        };
        let client = reqwest::Client::builder()
            .redirect(redirect)
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .build()?;
        Ok(Self {
            auth,
            model,
            timeout,
            client,
            endpoint,
        })
    }
    pub fn subscription_store(&self) -> Option<&AuthStore> {
        match &self.auth {
            ResponsesAuth::Subscription(store) => Some(store),
            ResponsesAuth::ApiKey { .. } => None,
        }
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub async fn send(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        let model = options.model.as_deref().unwrap_or(&self.model);
        let (wire, access_token, session) = match &self.auth {
            ResponsesAuth::Subscription(store) => {
                let (account, token) = store.access(cancel).await?;
                if model.is_empty() {
                    bail!("Select AI_CHAT_SUBSCRIPTION_MODEL after chat_auth models.");
                }
                (
                    Wire::subscription(model, &account),
                    token,
                    Some((store, account)),
                )
            }
            ResponsesAuth::ApiKey {
                key,
                fingerprint,
                reasoning_effort,
            } => (
                Wire::api_key(model, &self.endpoint, fingerprint, reasoning_effort.clone()),
                key.clone(),
                None,
            ),
        };
        let mut body = build_body(messages, options, &wire)?;
        if !wire.is_subscription() {
            recovery::strip_known(&self.endpoint, model, &mut body);
        }
        let cancelled = || {
            cancel.is_some_and(|c| c())
                || session
                    .as_ref()
                    .is_some_and(|(store, account)| !store.is_active(account))
        };
        let mut visible = false;
        auth::wait(
            async {
                let mut attempt = 0;
                loop {
                    match self
                        .attempt(
                            &body,
                            options,
                            &wire,
                            &access_token,
                            &cancelled,
                            on_delta,
                            &mut visible,
                        )
                        .await
                    {
                        Ok(response) => return Ok(response),
                        Err(error)
                            if !visible
                                && !wire.is_subscription()
                                && recovery::drop_rejected(
                                    &self.endpoint,
                                    model,
                                    &error,
                                    &mut body,
                                )
                                .is_some() => {}
                        Err(error) => {
                            let retry = error.downcast_ref::<crate::ApiError>().is_some_and(|e| {
                                e.status.is_some_and(|s| s == 429 || s >= 500)
                                    && !e.message.contains("usage_limit")
                                    && !e.message.contains("user_not_eligible")
                            });
                            if visible || !retry || attempt == 3 {
                                return Err(error);
                            }
                            let delay = error
                                .downcast_ref::<crate::ApiError>()
                                .and_then(|e| e.retry_after)
                                .unwrap_or(Duration::from_millis(500 << attempt));
                            auth::wait(
                                async {
                                    tokio::time::sleep(delay).await;
                                    Ok(())
                                },
                                Some(&cancelled),
                                self.timeout,
                            )
                            .await?;
                            attempt += 1;
                        }
                    }
                }
            },
            Some(&cancelled),
            self.timeout,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        &self,
        body: &Value,
        options: &ChatRequestOptions,
        wire: &Wire,
        token: &str,
        cancel: &dyn Fn() -> bool,
        on_delta: &mut dyn FnMut(&str),
        visible: &mut bool,
    ) -> Result<Value> {
        let mut attempt_usage = AttemptUsage(false);
        let response = auth::wait(
            async {
                self.client
                    .post(&self.endpoint)
                    .bearer_auth(token)
                    .json(body)
                    .send()
                    .await
                    .map_err(|_| {
                        crate::ApiError {
                            status: Some(503),
                            retry_after: None,
                            message: "Responses connection failed.".into(),
                        }
                        .into()
                    })
            },
            Some(cancel),
            self.timeout,
        )
        .await?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);
        if !status.is_success() {
            let message = if wire.is_subscription() {
                auth::checked_json(response)
                    .await
                    .err()
                    .unwrap_or_else(|| anyhow!("Responses HTTP failure."))
                    .to_string()
            } else {
                // An API-key endpoint's own message (unknown model, bad
                // parameter) is what the operator needs, as on chat/completions.
                // `param` names the rejected field for `recovery`.
                let text = response.text().await.unwrap_or_default();
                let message = crate::client::error_message_from_body(&text, status);
                match serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v.pointer("/error/param")?.as_str().map(str::to_owned))
                {
                    Some(param) => format!("{message} (param={})", auth::identifier(Some(&param))),
                    None => message,
                }
            };
            return Err(crate::ApiError {
                status: Some(status.as_u16()),
                retry_after,
                message,
            }
            .into());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if !content_type.starts_with("text/event-stream") {
            // A gateway that ignores `stream: true` answers with the final
            // response object itself; accept it like chat/completions does.
            if !wire.is_subscription() && content_type.starts_with("application/json") {
                let native: Value = auth::wait(
                    async {
                        response
                            .json()
                            .await
                            .map_err(|_| anyhow!("Invalid Responses JSON body."))
                    },
                    Some(cancel),
                    self.timeout,
                )
                .await?;
                attempt_usage.completed(&native);
                return finish(&native, options, wire);
            }
            bail!("Responses API requires an SSE stream.");
        }
        let mut stream = response.bytes_stream();
        let mut splitter = crate::stream::SseFrameSplitter::new();
        let mut bytes = 0usize;
        while let Some(chunk) = auth::wait(
            async {
                stream.next().await.transpose().map_err(|_| {
                    crate::ApiError {
                        status: Some(503),
                        retry_after: None,
                        message: "Responses stream disconnected.".into(),
                    }
                    .into()
                })
            },
            Some(cancel),
            self.timeout.min(Duration::from_secs(60)),
        )
        .await?
        {
            bytes = bytes.saturating_add(chunk.len());
            if bytes > 16 * 1024 * 1024 {
                bail!("Responses stream exceeded the size limit.");
            }
            for frame in splitter.push(&chunk) {
                let event: Value = serde_json::from_str(&frame)
                    .map_err(|_| anyhow!("Invalid Responses SSE event."))?;
                match event.get("type").and_then(Value::as_str) {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = event.get("delta").and_then(Value::as_str)
                            && !delta.is_empty()
                        {
                            *visible = true;
                            on_delta(delta);
                        }
                    }
                    Some("response.completed") => {
                        let native = event
                            .get("response")
                            .ok_or_else(|| anyhow!("Responses completion has no response."))?;
                        attempt_usage.completed(native);
                        let normalized = normalize(native, options, wire)?;
                        return Ok(normalized);
                    }
                    Some("response.incomplete") if !wire.is_subscription() => {
                        let native = event
                            .get("response")
                            .ok_or_else(|| anyhow!("Responses completion has no response."))?;
                        attempt_usage.completed(native);
                        return finish(native, options, wire);
                    }
                    Some(kind @ ("response.failed" | "response.incomplete" | "error")) => {
                        let code = auth::identifier(
                            event
                                .pointer("/response/error/code")
                                .and_then(Value::as_str)
                                .or_else(|| event.get("code").and_then(Value::as_str)),
                        );
                        let hint = if !wire.is_subscription() {
                            event
                                .pointer("/response/error/message")
                                .or_else(|| event.get("message"))
                                .and_then(Value::as_str)
                                .map(|m| format!(" {}", crate::client::truncate_for_display(m)))
                                .unwrap_or_default()
                        } else if code.contains("usage_limit") {
                            " See https://chatgpt.com/settings/usage. No API-key fallback.".into()
                        } else {
                            String::new()
                        };
                        return Err(crate::ApiError {
                            status: (kind != "response.incomplete" && code == "server_error")
                                .then_some(500),
                            retry_after: None,
                            message: format!("Responses did not complete (code={code}).{hint}"),
                        }
                        .into());
                    }
                    _ => {}
                }
            }
        }
        bail!(
            "Responses stream ended without response.completed; partial output is not a successful answer."
        );
    }
}

/// Normalize a terminal API-key response, naming why an unfinished one stopped.
fn finish(native: &Value, options: &ChatRequestOptions, wire: &Wire) -> Result<Value> {
    match native.get("status").and_then(Value::as_str) {
        Some("completed" | "incomplete") => normalize(native, options, wire),
        _ => {
            let code = auth::identifier(native.pointer("/error/code").and_then(Value::as_str));
            let detail = native
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(|m| format!(" {}", crate::client::truncate_for_display(m)))
                .unwrap_or_default();
            bail!("Responses did not complete (code={code}).{detail}")
        }
    }
}

impl ResponsesTransport {
    pub async fn models(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<Vec<Value>> {
        self.models_at("https://api.openai.com/v1/models", cancel)
            .await
    }
    // Internal seam for mock HTTP tests; public catalog routing stays fixed.
    async fn models_at(
        &self,
        endpoint: &str,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<Value>> {
        let store = self
            .subscription_store()
            .ok_or_else(|| anyhow!("The model catalog needs ChatGPT subscription."))?;
        let (_, token) = store.access(cancel).await?;
        auth::wait(async {
            let response = self.client.get(endpoint).bearer_auth(token).send().await
                .map_err(|_| anyhow!("Cannot fetch ChatGPT model catalog."))?;
            let data = auth::checked_json(response).await?;
            let models = data.get("models").and_then(Value::as_array).ok_or_else(|| anyhow!("ChatGPT model catalog has no models array."))?;
            Ok(models.iter().filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
                .filter_map(|m| Some(json!({"slug": m.get("slug")?.as_str()?, "display_name": m.get("display_name")?.as_str()?}))).collect())
        }, cancel, self.timeout).await
    }
}
