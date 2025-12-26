use anyhow::{Error, Result, anyhow};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tracing::debug;

use crate::config::OpenAiConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
pub const CANCELLED_MESSAGE: &str = "OpenAI request cancelled by Ctrl+C";

#[derive(Debug)]
struct RequestCancelled;

impl fmt::Display for RequestCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CANCELLED_MESSAGE)
    }
}

impl std::error::Error for RequestCancelled {}

/// Returns true when the provided error represents a Ctrl+C cancellation
/// triggered during an OpenAI request.
pub fn is_ctrl_c_cancelled(err: &Error) -> bool {
    err.downcast_ref::<RequestCancelled>().is_some()
}

#[derive(Debug, Clone)]
pub struct ChatGptClient {
    api_key: String,
    default_model: String,
    chat_endpoint: String,
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
        };

        let _ = client.build_client()?;
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
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let prompt_clone = prompt.clone();
            let model_clone = model.clone();
            tokio::task::block_in_place(move || {
                handle.block_on(self.send_message_inner(
                    input,
                    prompt_clone,
                    temperature,
                    model_clone,
                    cancel_check,
                ))
            })
        } else {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(self.send_message_inner(
                input,
                prompt,
                temperature,
                model,
                cancel_check,
            ))
        }
    }

    pub fn send_chat_request(
        &self,
        messages: &[Value],
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<&[Value]>,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<Value> {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let messages_vec = messages.to_vec();
            let tools_vec = tools.map(|items| items.to_vec());
            let model_clone = model.clone();
            tokio::task::block_in_place(move || {
                handle.block_on(self.send_chat_request_inner(
                    messages_vec,
                    temperature,
                    model_clone,
                    tools_vec,
                    cancel_check,
                ))
            })
        } else {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(self.send_chat_request_inner(
                messages.to_vec(),
                temperature,
                model,
                tools.map(|items| items.to_vec()),
                cancel_check,
            ))
        }
    }

    async fn send_message_inner(
        &self,
        content: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        model: Option<String>,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<String> {
        let builder_messages = Self::build_messages(content, prompt);
        let data = self
            .send_chat_request_inner(builder_messages, temperature, model, None, cancel_check)
            .await?;
        let output = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("Unexpected response {data}"))?;

        Ok(output.to_string())
    }

    async fn send_chat_request_inner(
        &self,
        messages: Vec<Value>,
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<Vec<Value>>,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<Value> {
        let builder = self.request_builder_from_messages(messages, temperature, model, tools)?;
        let response = Self::await_with_cancel(builder.send(), cancel_check).await?;
        let data: Value = Self::await_with_cancel(response.json(), cancel_check).await?;

        let choices_len = data
            .get("choices")
            .and_then(|choices| choices.as_array())
            .map(|choices| choices.len())
            .unwrap_or(0);
        let has_error = data.get("error").is_some();
        debug!(
            chat_direction = "response",
            choices = choices_len,
            has_error = has_error
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

        // Attempt to listen for Ctrl+C, but don't fail if we can't (e.g. if a handler is already set)
        // If handler registration fails, this future will just be ignored in the select! loop
        let ctrl_c_future = async {
            match tokio::signal::ctrl_c().await {
                Ok(()) => true,
                Err(e) => {
                    debug!("dsh-openai: Failed to listen for Ctrl+C via tokio: {}", e);
                    std::future::pending::<bool>().await
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

    fn build_client(&self) -> Result<Client> {
        let client = Client::builder().timeout(CONNECT_TIMEOUT).build()?;
        Ok(client)
    }

    fn request_builder_from_messages(
        &self,
        messages: Vec<Value>,
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<Vec<Value>>,
    ) -> Result<RequestBuilder> {
        // Use provided model or fall back to default
        let selected_model = model.unwrap_or_else(|| self.default_model.clone());

        let mut body = json!({
            "model": selected_model,
            "messages": messages,
        });

        let tool_count = tools.as_ref().map(|items| items.len()).unwrap_or(0);

        if let Some(v) = temperature
            && let Some(map) = body.as_object_mut()
        {
            map.insert("temperature".into(), json!(v));
        }

        if let Some(tools) = tools
            && let Some(map) = body.as_object_mut()
        {
            map.insert("tools".into(), json!(tools));
        }

        debug!(
            chat_direction = "request",
            model = %selected_model,
            message_count = messages.len(),
            tool_count = tool_count,
            temperature = ?temperature
        );

        let header_value = format!("Bearer {}", &self.api_key);
        let builder = self
            .build_client()?
            .post(&self.chat_endpoint)
            .header("Authorization", header_value)
            .json(&body);

        Ok(builder)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::{Duration, sleep};

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
}
