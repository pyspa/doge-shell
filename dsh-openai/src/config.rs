/// Primary key for the chat endpoint path segment.
const CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// Default base URL for OpenAI-compatible APIs.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/";

/// Default model used when none is provided.
pub const DEFAULT_MODEL: &str = "gpt-5-mini";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiConfig {
    api_key: Option<String>,
    base_url: String,
    default_model: String,
}

impl OpenAiConfig {
    pub fn new(
        api_key: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
    ) -> Self {
        let base_url = sanitize_base_url(base_url);
        let default_model = default_model
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        Self {
            api_key,
            base_url,
            default_model,
        }
    }

    pub fn from_getter(mut getter: impl FnMut(&str) -> Option<String>) -> Self {
        let api_key = getter("AI_CHAT_API_KEY")
            .or_else(|| getter("OPENAI_API_KEY"))
            .or_else(|| getter("OPEN_AI_API_KEY"));

        let base_url = getter("AI_CHAT_BASE_URL").or_else(|| getter("OPENAI_BASE_URL"));

        let default_model = getter("AI_CHAT_MODEL").or_else(|| getter("OPENAI_MODEL"));

        OpenAiConfig::new(api_key, base_url, default_model)
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn chat_endpoint(&self) -> String {
        build_chat_endpoint(&self.base_url)
    }

    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key;
        self
    }
}

fn sanitize_base_url(base_url: Option<String>) -> String {
    base_url
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.trim_end_matches('/').to_string())
            }
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.trim_end_matches('/').to_string())
}

fn build_chat_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(CHAT_COMPLETIONS_PATH) {
        trimmed.to_string()
    } else {
        format!("{trimmed}/{CHAT_COMPLETIONS_PATH}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_base_url_defaults_when_none() {
        let cfg = OpenAiConfig::new(None, None, None);
        assert_eq!(cfg.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn build_chat_endpoint_handles_existing_path() {
        let cfg = OpenAiConfig::new(
            None,
            Some("https://example.com/v1/chat/completions".to_string()),
            None,
        );
        assert_eq!(
            cfg.chat_endpoint(),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn build_chat_endpoint_appends_path() {
        let cfg = OpenAiConfig::new(None, Some("https://example.com/v1/".to_string()), None);
        assert_eq!(
            cfg.chat_endpoint(),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn from_getter_prefers_primary_keys() {
        let getter = |key: &str| match key {
            "AI_CHAT_API_KEY" => Some("primary".to_string()),
            "OPENAI_API_KEY" => Some("legacy".to_string()),
            "AI_CHAT_BASE_URL" => Some("https://example.com/api/".to_string()),
            "AI_CHAT_MODEL" => Some("primary-model".to_string()),
            "OPENAI_MODEL" => Some("legacy-model".to_string()),
            _ => None,
        };

        let cfg = OpenAiConfig::from_getter(getter);

        assert_eq!(cfg.api_key(), Some("primary"));
        assert_eq!(cfg.base_url(), "https://example.com/api");
        assert_eq!(cfg.default_model(), "primary-model");
    }

    #[test]
    fn from_getter_supports_double_underscored_legacy_key() {
        let getter = |key: &str| match key {
            "OPEN_AI_API_KEY" => Some("legacy".to_string()),
            _ => None,
        };

        let cfg = OpenAiConfig::from_getter(getter);

        assert_eq!(cfg.api_key(), Some("legacy"));
        assert_eq!(cfg.base_url(), "https://api.openai.com/v1");
        assert_eq!(cfg.default_model(), DEFAULT_MODEL);
    }
}
