//! Fixed OpenCode routes and an offline Chat Completions catalog.
//! Sources (2026-10-15): https://opencode.ai/v2/docs/console/go/#endpoints
//! and https://opencode.ai/v2/docs/console/models/#endpoints.
use crate::AiProvider;

pub const GO_MODEL_ENV: &str = "AI_CHAT_OPENCODE_GO_MODEL";
pub const ZEN_MODEL_ENV: &str = "AI_CHAT_OPENCODE_ZEN_MODEL";
pub const GO_KEY_ENV: &str = "OPENCODE_GO_API_KEY";
pub const ZEN_KEY_ENV: &str = "OPENCODE_ZEN_API_KEY";
pub const SHARED_KEY_ENV: &str = "OPENCODE_API_KEY";

impl AiProvider {
    pub fn is_opencode(self) -> bool {
        matches!(self, Self::OpencodeGo | Self::OpencodeZen)
    }
    pub fn opencode_model_env(self) -> Option<&'static str> {
        match self {
            Self::OpencodeGo => Some(GO_MODEL_ENV),
            Self::OpencodeZen => Some(ZEN_MODEL_ENV),
            _ => None,
        }
    }
    pub fn opencode_key_env(self) -> Option<&'static str> {
        match self {
            Self::OpencodeGo => Some(GO_KEY_ENV),
            Self::OpencodeZen => Some(ZEN_KEY_ENV),
            _ => None,
        }
    }
    pub fn opencode_base(self) -> Option<&'static str> {
        match self {
            Self::OpencodeGo => Some("https://opencode.ai/inference/go/v1"),
            Self::OpencodeZen => Some("https://opencode.ai/inference/v1"),
            _ => None,
        }
    }
    /// A verified subset, not an entitlement or live availability check.
    pub fn opencode_models(self) -> &'static [&'static str] {
        match self {
            Self::OpencodeGo => &[
                "glm-5.3-flash",
                "glm-5.3",
                "glm-5.2",
                "kimi-k3",
                "kimi-k2.7-code",
                "kimi-k2.6",
                "longcat-2.0",
                "deepseek-v4.1-flash",
                "deepseek-v4-pro",
                "mimo-v2.6-flash",
                "mimo-v2.6-pro",
                "mimo-v2.5",
                "mimo-v2.5-pro",
                "hy4-preview",
                "hy3",
                "space-bunny",
                "longcat-2.5-preview-free",
                "step-5-preview-free",
                "cat-jam-free",
            ],
            Self::OpencodeZen => &[
                "qwen3.8-max",
                "deepseek-v4.1-flash",
                "deepseek-v4-pro",
                "minimax-m3",
                "minimax-m2.7",
                "minimax-m2.5",
                "glm-5.3-flash",
                "glm-5.3",
                "glm-5.2",
                "glm-5.1",
                "glm-5",
                "kimi-k2.5",
                "kimi-k2.6",
                "kimi-k2.7-code",
                "kimi-k3",
                "mistral-large-4",
                "big-pickle",
                "deepseek-v4.1-flash-free",
                "space-bunny-free",
                "cat-jam-free",
                "longcat-2.5-preview-free",
                "step-5-preview-free",
                "exo-free",
                "mimo-v2.6-flash-free",
            ],
            _ => &[],
        }
    }
    pub fn validate_opencode_model(self, model: &str) -> anyhow::Result<()> {
        if !self.is_opencode() {
            return Ok(());
        }
        if model.is_empty() {
            anyhow::bail!(
                "Select {} and a model from chat_provider models {}. No default model is selected.",
                self.opencode_model_env().unwrap(),
                self.name()
            );
        }
        if !self.opencode_models().contains(&model) {
            anyhow::bail!(
                "Model is outside the verified {} Chat Completions catalog (2026-10-15). Use chat_provider models {}. Responses, Messages and Google protocols are not supported by this provider.",
                self.name(),
                self.name()
            );
        }
        Ok(())
    }
    pub fn opencode_billing(self) -> Option<&'static str> {
        match self {
            Self::OpencodeGo => Some(
                "Go subscription quota; Console Use balance may spend prepaid balance after quota. No automatic Zen fallback.",
            ),
            Self::OpencodeZen => {
                Some("Zen / Console prepaid, pay-as-you-go; separate from Go subscription quota.")
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OpenAiConfig, PROVIDER_ENV};
    #[test]
    fn provider_routes_and_keys_are_isolated_and_do_not_mutate_generic_settings() {
        let mut vars = std::collections::HashMap::from([
            (PROVIDER_ENV, "opencode_go"),
            (GO_MODEL_ENV, "glm-5.3"),
            (ZEN_MODEL_ENV, "qwen3.8-max"),
            (GO_KEY_ENV, "go-secret"),
            (ZEN_KEY_ENV, "zen-secret"),
            (SHARED_KEY_ENV, "shared-secret"),
            ("AI_CHAT_API_KEY", "generic-secret"),
            ("AI_CHAT_MODEL", "generic-model"),
            ("AI_CHAT_BASE_URL", "https://generic.example/v1"),
        ]);
        for (provider, key, model, base) in [
            (
                "opencode_go",
                "go-secret",
                "glm-5.3",
                "https://opencode.ai/inference/go/v1",
            ),
            (
                "opencode_zen",
                "zen-secret",
                "qwen3.8-max",
                "https://opencode.ai/inference/v1",
            ),
            (
                "api_key",
                "generic-secret",
                "generic-model",
                "https://generic.example/v1",
            ),
        ] {
            vars.insert(PROVIDER_ENV, provider);
            let config = OpenAiConfig::from_getter(|k| vars.get(k).map(|v| (*v).into()));
            config.readiness().unwrap();
            assert_eq!(config.api_key(), Some(key));
            assert_eq!(config.default_model(), model);
            assert_eq!(config.base_url(), base);
            assert!(!format!("{config:?}").contains(key));
        }
        vars.insert(PROVIDER_ENV, "opencode_go");
        vars.insert(GO_KEY_ENV, "");
        let config = OpenAiConfig::from_getter(|k| vars.get(k).map(|v| (*v).into()));
        assert_eq!(config.api_key(), Some("shared-secret"));
        vars.remove(SHARED_KEY_ENV);
        let config = OpenAiConfig::from_getter(|k| vars.get(k).map(|v| (*v).into()));
        assert!(config.api_key().is_none());
        assert!(config.readiness().is_err());
        vars.insert(GO_MODEL_ENV, "qwen3.8-max");
        assert!(
            OpenAiConfig::from_getter(|k| vars.get(k).map(|v| (*v).into()))
                .validate()
                .is_err()
        );
    }
}
