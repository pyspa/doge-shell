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
    ) -> Result<String> {
        self.send_message_with_model(input, prompt, temperature, None)
    }

    pub fn send_message_with_model(
        &self,
        input: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        model: Option<String>,
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
                ))
            })
        } else {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(self.send_message_inner(input, prompt, temperature, model))
        }
    }

    pub fn send_chat_request(
        &self,
        messages: &[Value],
        temperature: Option<f64>,
        model: Option<String>,
        tools: Option<&[Value]>,
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
                ))
            })
        } else {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(self.send_chat_request_inner(
                messages.to_vec(),
                temperature,
                model,
                tools.map(|items| items.to_vec()),
            ))
        }
    }

    async fn send_message_inner(
        &self,
        content: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        model: Option<String>,
    ) -> Result<String> {
        let builder_messages = Self::build_messages(content, prompt);
        let data = self
            .send_chat_request_inner(builder_messages, temperature, model, None)
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
    ) -> Result<Value> {
        let builder = self.request_builder_from_messages(messages, temperature, model, tools)?;
        let response = Self::await_with_ctrl_c(builder.send()).await?;
        let data: Value = Self::await_with_ctrl_c(response.json()).await?;

        match serde_json::to_string(&data) {
            Ok(serialized) => debug!(chat_direction = "response", json = %serialized),
            Err(err) => debug!(
                chat_direction = "response",
                "chat_response_serialization_failed: {err}"
            ),
        }

        Ok(data)
    }

    async fn await_with_ctrl_c<F, T, E>(future: F) -> Result<T>
    where
        F: Future<Output = Result<T, E>>,
        anyhow::Error: From<E>,
    {
        tokio::pin!(future);

        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);

        tokio::select! {
            res = &mut future => res.map_err(anyhow::Error::from),
            ctrl = &mut ctrl_c => match ctrl {
                Ok(()) => Err(RequestCancelled.into()),
                Err(e) => Err(anyhow!("Failed to listen for Ctrl+C: {e}")),
            },
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

        match serde_json::to_string(&body) {
            Ok(serialized) => debug!(chat_direction = "request", json = %serialized),
            Err(err) => debug!(
                chat_direction = "request",
                "chat_request_serialization_failed: {err}"
            ),
        }

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
