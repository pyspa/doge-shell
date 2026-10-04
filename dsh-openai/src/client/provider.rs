//! Provider construction, cache identity, and credential-free diagnostics.
use super::*;
impl ChatGptClient {
    pub fn try_from_config(config: &OpenAiConfig) -> Result<Self> {
        config.validate()?;
        let subscription = if config.provider() == crate::AiProvider::ChatGptSubscription {
            Some(crate::responses::SubscriptionTransport::new(
                crate::auth::AuthStore::new(config.auth_dir()?.to_path_buf()),
                config.default_model().into(),
                config.timeout(),
            )?)
        } else {
            None
        };
        let api_key = if subscription.is_some() {
            ""
        } else {
            config.api_key().ok_or_else(|| {
                anyhow!(
                    "OpenAI-compatible API key is not configured. {}",
                    crate::API_KEY_SETUP_HINT
                )
            })?
        };

        let client = Self {
            subscription,
            api_key: api_key.to_string(),
            default_model: config.default_model().to_string(),
            chat_endpoint: config.chat_endpoint(),
            client: Self::build_client(config.timeout())?,
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
        if let Some(subscription) = &self.subscription {
            "chatgpt_subscription".hash(&mut hash);
            if let Ok(account) = subscription.store.status() {
                account.label.hash(&mut hash);
                account.client_id.hash(&mut hash);
                account.subject.hash(&mut hash);
            } else {
                "unavailable".hash(&mut hash);
            }
        } else {
            "api_key".hash(&mut hash);
            self.api_key.hash(&mut hash);
            self.chat_endpoint.hash(&mut hash);
        }
        format!("{:016x}", hash.finish())
    }
    pub(super) fn check_api_history(messages: &[Value]) -> Result<()> {
        if messages
            .iter()
            .any(|m| m.get(crate::responses::CONTINUATION).is_some())
        {
            anyhow::bail!(
                "This history uses ChatGPT subscription. Run chat_reset before changing provider."
            );
        }
        Ok(())
    }
    pub fn subscription_models(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<Vec<Value>> {
        let transport = self.subscription.as_ref().ok_or_else(|| {
            anyhow!("Select AI_CHAT_PROVIDER=chatgpt_subscription to list subscription models.")
        })?;
        self.block_on(transport.models(cancel))?
    }
}

impl fmt::Debug for ChatGptClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatGptClient")
            .field("default_model", &self.default_model)
            .field("subscription", &self.subscription.is_some())
            .finish_non_exhaustive()
    }
}
