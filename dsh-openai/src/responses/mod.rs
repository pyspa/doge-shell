//! Stateless OAuth Responses transport; normalized consumers and native history are distinct.
use crate::{
    ChatRequestOptions,
    auth::{self, Account, AuthStore},
};
use anyhow::{Result, anyhow, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
#[cfg(test)]
mod tests;
mod wire;
use wire::{build_body, normalize};
pub(crate) const CONTINUATION: &str = "_dsh_responses";

#[derive(Clone, Debug)]
pub(crate) struct SubscriptionTransport {
    pub store: AuthStore,
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
impl SubscriptionTransport {
    pub fn new(store: AuthStore, model: String, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .build()?;
        Ok(Self {
            store,
            model,
            timeout,
            client,
            endpoint: "https://api.openai.com/v1/responses".into(),
        })
    }
    pub async fn send(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        let (account, access_token) = self.store.access(cancel).await?;
        let model = options.model.as_deref().unwrap_or(&self.model);
        if model.is_empty() {
            bail!("Select AI_CHAT_SUBSCRIPTION_MODEL after chat_auth models.");
        }
        let body = build_body(messages, options, model, &account)?;
        let cancelled = || cancel.is_some_and(|c| c()) || !self.store.is_active(&account);
        let mut visible = false;
        auth::wait(
            async {
                for attempt in 0..=3 {
                    match self
                        .attempt(
                            &body,
                            options,
                            model,
                            &account,
                            &access_token,
                            &cancelled,
                            on_delta,
                            &mut visible,
                        )
                        .await
                    {
                        Ok(response) => return Ok(response),
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
                        }
                    }
                }
                unreachable!()
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
        model: &str,
        account: &Account,
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
            let error = auth::checked_json(response)
                .await
                .err()
                .unwrap_or_else(|| anyhow!("Responses HTTP failure."));
            return Err(crate::ApiError {
                status: Some(status.as_u16()),
                retry_after,
                message: error.to_string(),
            }
            .into());
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            bail!("ChatGPT Responses requires an SSE stream.");
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
                bail!("ChatGPT Responses stream exceeded the size limit.");
            }
            for frame in splitter.push(&chunk) {
                let event: Value = serde_json::from_str(&frame)
                    .map_err(|_| anyhow!("Invalid ChatGPT Responses SSE event."))?;
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
                        let normalized = normalize(native, options, model, account)?;
                        return Ok(normalized);
                    }
                    Some(kind @ ("response.failed" | "response.incomplete" | "error")) => {
                        let code = auth::identifier(
                            event
                                .pointer("/response/error/code")
                                .and_then(Value::as_str)
                                .or_else(|| event.get("code").and_then(Value::as_str)),
                        );
                        let hint = if code.contains("usage_limit") {
                            " See https://chatgpt.com/settings/usage. No API-key fallback."
                        } else {
                            ""
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
            "ChatGPT Responses stream ended without response.completed; partial output is not a successful answer."
        );
    }
    pub async fn models(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<Vec<Value>> {
        let (_, token) = self.store.access(cancel).await?;
        auth::wait(async {
            let response = self.client.get("https://api.openai.com/v1/models").bearer_auth(token).send().await
                .map_err(|_| anyhow!("Cannot fetch ChatGPT model catalog."))?;
            let data = auth::checked_json(response).await?;
            let models = data.get("models").and_then(Value::as_array).ok_or_else(|| anyhow!("ChatGPT model catalog has no models array."))?;
            Ok(models.iter().filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
                .filter_map(|m| Some(json!({"slug": m.get("slug")?.as_str()?, "display_name": m.get("display_name")?.as_str()?}))).collect())
        }, cancel, self.timeout).await
    }
}
