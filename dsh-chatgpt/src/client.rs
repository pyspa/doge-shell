use anyhow::{anyhow, Result};
use async_std::task;
use serde_json::{json, Value};
use std::time::Duration;
use surf::{Client, Config, RequestBuilder, Url};
use tracing::debug;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const API_URL: &str = "https://api.openai.com/v1/chat/completions";
const MODEL: &str = "gpt-3.5-turbo";

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
        task::block_on(f)
    }

    async fn send_message_inner(
        &self,
        content: &str,
        prompt: Option<String>,
        temperature: Option<f64>,
    ) -> Result<String> {
        let builder = self.request_builder(content, prompt, temperature)?;

        let mut res = builder.await.unwrap();
        let data: Value = res.body_json().await.unwrap();
        let output = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("Unexpected response {data}"))?;

        Ok(output.to_string())
    }

    fn build_client(&self) -> Result<Client> {
        let config = Config::new();
        let client = config
            .set_base_url(Url::parse(API_URL)?)
            .set_timeout(Some(CONNECT_TIMEOUT))
            .try_into()?;
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
            .body_json(&body)
            .unwrap(); // TODO check err

        Ok(builder)
    }
}
