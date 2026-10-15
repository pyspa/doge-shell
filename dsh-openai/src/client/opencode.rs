//! Request-local OpenCode identity, shared by retry and both response modes.
use super::*;

impl ChatGptClient {
    pub(super) fn for_request(&self, options: &ChatRequestOptions) -> Result<Self> {
        let mut request = self.clone();
        if self.provider.is_opencode() {
            self.provider
                .validate_opencode_model(options.model.as_deref().unwrap_or(&self.default_model))?;
            use sha2::{Digest, Sha256};
            let identity = options
                .session_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            request.opencode_session = Some(format!("{:x}", Sha256::digest(identity.as_bytes())));
        }
        Ok(request)
    }
    pub(super) fn request_builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .post(&self.chat_endpoint)
            .bearer_auth(&self.api_key)
            .json(body);
        if let Some(session) = &self.opencode_session {
            builder = builder
                .header(
                    reqwest::header::USER_AGENT,
                    concat!("doge-shell/", env!("CARGO_PKG_VERSION")),
                )
                .header("x-opencode-session", session);
        }
        builder
    }
    pub(super) fn safe_server_message(&self, message: String) -> String {
        if self.provider.is_opencode() {
            message.replace(&self.api_key, "[redacted]")
        } else {
            message
        }
    }
    pub(super) fn server_error_value(&self, data: &Value) -> Option<String> {
        if !self.provider.is_opencode() {
            return error_message_from_value(data);
        }
        let error = data.get("error")?;
        if error.is_null() {
            return None;
        }
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        Some(truncate_for_display(&self.safe_server_message(message)))
    }
    pub(super) fn server_error_body(&self, text: &str, status: StatusCode) -> String {
        if !self.provider.is_opencode() {
            return error_message_from_body(text, status);
        }
        if let Ok(data) = serde_json::from_str::<Value>(text)
            && let Some(message) = self.server_error_value(&data)
        {
            return message;
        }
        error_message_from_body(&self.safe_server_message(text.into()), status)
    }
    pub(super) fn provider_error(&self, error: Error) -> Error {
        if !self.provider.is_opencode() {
            return error;
        }
        // Keep cancellation and typed retry metadata intact. Server text is untrusted;
        // redact a reflected credential before adding local routing guidance.
        if let Some(api) = error.downcast_ref::<ApiError>() {
            let hint = match api.status {
                Some(401 | 403) => "Check the OpenCode API key and model access in Console.",
                Some(429) => self.provider.opencode_billing().unwrap(),
                _ => "Check OpenCode service availability and the supported model catalog.",
            };
            return ApiError {
                status: api.status,
                retry_after: api.retry_after,
                message: format!(
                    "{}: {} {}",
                    self.provider.name(),
                    api.message.replace(&self.api_key, "[redacted]"),
                    hint
                ),
            }
            .into();
        }
        error
    }
}
