use anyhow::{Result, anyhow};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::time::Duration;
use tracing::debug;

use crate::config::OpenAiConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
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

    async fn send_message_inner(
        &self,
        content: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        model: Option<String>,
    ) -> Result<String> {
        let builder = self.request_builder(content, prompt, temperature, model)?;

        let res = builder.send().await?;
        let data: Value = res.json().await?;
        let output = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("Unexpected response {data}"))?;

        Ok(output.to_string())
    }

    fn build_client(&self) -> Result<Client> {
        let client = Client::builder().timeout(CONNECT_TIMEOUT).build()?;
        Ok(client)
    }

    fn request_builder(
        &self,
        content: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
        model: Option<String>,
    ) -> Result<RequestBuilder> {
        let user_message = json!({ "role": "user", "content": content });
        let messages = match prompt {
            Some(prompt) => {
                let system_message = json!({ "role": "system", "content": prompt.trim() });
                json!([system_message, user_message])
            }
            None => {
                json!([user_message])
            }
        };

        // Use provided model or fall back to default
        let selected_model = model.unwrap_or_else(|| self.default_model.clone());

        let mut body = json!({
            "model": selected_model,
            "messages": messages,
        });

        if let Some(v) = temperature {
            body.as_object_mut()
                .and_then(|m| m.insert("temperature".into(), json!(v)));
        }

        debug!("req: {:?}", body);

        let header_value = format!("Bearer {}", &self.api_key);
        let builder = self
            .build_client()?
            .post(&self.chat_endpoint)
            .header("Authorization", header_value)
            .json(&body);

        Ok(builder)
    }
}
