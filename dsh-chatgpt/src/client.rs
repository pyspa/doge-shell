use anyhow::{Result, anyhow};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::time::Duration;
use tracing::debug;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const API_URL: &str = "https://api.openai.com/v1/chat/completions";
const MODEL: &str = "gpt-4";

#[derive(Debug)]
pub struct ChatGptClient {
    api_key: String,
}

impl ChatGptClient {
    pub fn new(api_key: String) -> Result<Self> {
        let s = Self { api_key };
        let _ = s.build_client()?; // check error
        Ok(s)
    }

    pub fn send_message(
        &self,
        input: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
    ) -> Result<String> {
        let f = self.send_message_inner(input, prompt, temperature);
        tokio::runtime::Handle::current().block_on(f)
    }

    async fn send_message_inner(
        &self,
        content: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
    ) -> Result<String> {
        let builder = self.request_builder(content, prompt, temperature)?;

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

        let mut body = json!({
            "model": MODEL,
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
            .post(API_URL)
            .header("Authorization", header_value)
            .json(&body);

        Ok(builder)
    }
}
