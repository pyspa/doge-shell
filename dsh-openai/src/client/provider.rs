//! Provider construction, cache identity, and credential-free diagnostics.
use super::*;
impl ChatGptClient {
    pub fn try_from_config(config: &OpenAiConfig) -> Result<Self> {
        config.validate()?;
        let subscription = config.provider() == crate::AiProvider::ChatGptSubscription;
        if config.provider().is_opencode() {
            config.readiness()?;
        }
        let api_key = if subscription {
            ""
        } else {
            config.api_key().ok_or_else(|| {
                anyhow!(
                    "OpenAI-compatible API key is not configured. {}",
                    crate::API_KEY_SETUP_HINT
                )
            })?
        };
        let responses = if subscription {
            Some(crate::responses::ResponsesTransport::new(
                crate::auth::AuthStore::new(config.auth_dir()?.to_path_buf()),
                config.default_model().into(),
                config.timeout(),
            )?)
        } else if config.api_protocol() == crate::ApiProtocol::Responses {
            Some(crate::responses::ResponsesTransport::api_key(
                api_key.into(),
                config.responses_endpoint(),
                config.default_model().into(),
                config.timeout(),
                config.reasoning_effort().map(str::to_string),
            )?)
        } else {
            None
        };

        let client = Self {
            responses,
            provider: config.provider(),
            opencode_session: None,
            api_key: api_key.to_string(),
            default_model: config.default_model().to_string(),
            chat_endpoint: config.chat_endpoint(),
            client: if config.provider().is_opencode() {
                Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    .timeout(config.timeout())
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?
            } else {
                Self::build_client(config.timeout())?
            },
            request_timeout: config.timeout(),
            unsupported: Arc::new(Mutex::new(Vec::new())),
            default_reasoning_effort: config.reasoning_effort().map(str::to_string),
            force_reasoning_none: Arc::new(AtomicBool::new(false)),
        };
        Ok(client)
    }

    pub fn cache_scope(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.default_model.hash(&mut hash);
        if let Some(store) = self
            .responses
            .as_ref()
            .and_then(|responses| responses.subscription_store())
        {
            "chatgpt_subscription".hash(&mut hash);
            if let Ok(account) = store.status() {
                account.label.hash(&mut hash);
                account.client_id.hash(&mut hash);
                account.subject.hash(&mut hash);
            } else {
                "unavailable".hash(&mut hash);
            }
        } else {
            self.provider.name().hash(&mut hash);
            self.api_key.hash(&mut hash);
            match &self.responses {
                Some(responses) => responses.endpoint().hash(&mut hash),
                None => self.chat_endpoint.hash(&mut hash),
            }
        }
        format!("{:016x}", hash.finish())
    }
    pub(super) fn check_api_history(messages: &[Value]) -> Result<()> {
        if messages
            .iter()
            .any(|m| m.get(crate::responses::CONTINUATION).is_some())
        {
            anyhow::bail!(
                "This history uses the Responses API. Run chat_reset before switching to chat/completions."
            );
        }
        Ok(())
    }
    pub fn subscription_models(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<Vec<Value>> {
        let transport = self
            .responses
            .as_ref()
            .filter(|r| r.subscription_store().is_some())
            .ok_or_else(|| {
                anyhow!("Select AI_CHAT_PROVIDER=chatgpt_subscription to list subscription models.")
            })?;
        self.block_on(transport.models(cancel))?
    }
}

impl fmt::Debug for ChatGptClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatGptClient")
            .field("provider", &self.provider)
            .field("default_model", &self.default_model)
            .field("responses", &self.responses.is_some())
            .finish_non_exhaustive()
    }
}
