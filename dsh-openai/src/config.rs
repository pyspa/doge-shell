use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const PROVIDER_ENV: &str = "AI_CHAT_PROVIDER";
pub const SUBSCRIPTION_MODEL_ENV: &str = "AI_CHAT_SUBSCRIPTION_MODEL";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProvider {
    ApiKey,
    ChatGptSubscription,
}

/// Environment key overriding the total per-request timeout, in seconds.
pub const TIMEOUT_ENV: &str = "AI_CHAT_TIMEOUT_SECS";

/// Environment key for the `reasoning_effort` field sent with every request.
///
/// No value here is allow-listed: a reasoning-model generation the client does
/// not yet recognise still forwards whatever the operator set, and a value the
/// endpoint rejects costs one retry via `DROPPABLE_FIELDS` in `client.rs`
/// instead of failing to start.
pub const REASONING_EFFORT_ENV: &str = "AI_CHAT_REASONING_EFFORT";

/// API-key variables in resolution order.
pub const API_KEY_ENV_VARS: [&str; 3] = ["AI_CHAT_API_KEY", "OPENAI_API_KEY", "OPEN_AI_API_KEY"];

/// Shared user-facing guidance for configuring an API key.
pub const API_KEY_SETUP_HINT: &str =
    "Set AI_CHAT_API_KEY (preferred), OPENAI_API_KEY, or OPEN_AI_API_KEY.";

/// Primary key for the chat endpoint path segment.
const CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// Default base URL for OpenAI-compatible APIs.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/";

/// Default model used when none is provided.
pub const DEFAULT_MODEL: &str = "gpt-5-mini";
const ALLOW_INSECURE_HTTP_ENV: &str = "AI_CHAT_ALLOW_INSECURE_HTTP";

/// Total per-request budget. An agent turn with tools regularly needs more than
/// a minute, so the old fixed 60s cut long answers off mid-flight.
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;
const MIN_TIMEOUT_SECS: u64 = 5;
/// Ceiling `AI_CHAT_TIMEOUT_SECS` clamps to, and also the hard cap a
/// streaming request's per-request timeout override uses in `client.rs`:
/// a stream can legitimately outlast one non-streaming turn's budget, but
/// still needs a bound.
pub(crate) const MAX_TIMEOUT_SECS: u64 = 1800;

#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiConfig {
    provider: AiProvider,
    auth_dir: Option<PathBuf>,
    configuration_error: Option<String>,
    api_key: Option<String>,
    base_url: String,
    default_model: String,
    timeout: Duration,
    reasoning_effort: Option<String>,
}

impl OpenAiConfig {
    pub fn new(
        api_key: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
    ) -> Self {
        Self::new_with_http_policy(
            api_key,
            base_url,
            default_model,
            std::env::var(ALLOW_INSECURE_HTTP_ENV).ok(),
        )
    }

    fn new_with_http_policy(
        api_key: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
        allow_http: Option<String>,
    ) -> Self {
        let base_url = sanitize_base_url(base_url, allow_http);
        let default_model = default_model
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        Self {
            provider: AiProvider::ApiKey,
            auth_dir: None,
            configuration_error: None,
            // A blank key is not a key. Callers used to each re-check this and
            // `doctor` disagreed with the chat runtime about whether
            // `AI_CHAT_API_KEY=""` counted as configured.
            api_key: api_key.filter(|key| !key.trim().is_empty()),
            base_url,
            default_model,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            // Like `default_model` and `timeout` above: `new`/`new_with_http_policy`
            // take their value as an explicit argument or not at all, and never
            // reach into the process environment for it - only `from_getter`
            // does (via `REASONING_EFFORT_ENV`). `allow_http` above is the one
            // exception, because `sanitize_base_url` needs a same-call answer
            // to decide whether to silently replace an `http://` URL - a
            // security-relevant transform this constructor can't defer to a
            // caller who might not call `with_reasoning_effort` at all.
            reasoning_effort: None,
        }
    }

    pub fn from_getter(mut getter: impl FnMut(&str) -> Option<String>) -> Self {
        let provider = getter(PROVIDER_ENV).unwrap_or_else(|| "api_key".into());
        let subscription_model = getter(SUBSCRIPTION_MODEL_ENV).filter(|v| !v.trim().is_empty());
        let auth_dir = getter("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| getter("HOME").map(|v| PathBuf::from(v).join(".config")))
            .map(|v| v.join("dogesh").join("subscription-auth"));
        let api_key = API_KEY_ENV_VARS.iter().find_map(|key| getter(key));

        let base_url = getter("AI_CHAT_BASE_URL").or_else(|| getter("OPENAI_BASE_URL"));

        let default_model = getter("AI_CHAT_MODEL").or_else(|| getter("OPENAI_MODEL"));

        let timeout = getter(TIMEOUT_ENV)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|secs| secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS))
            .map(Duration::from_secs);

        let reasoning_effort = getter(REASONING_EFFORT_ENV).and_then(|value| {
            let trimmed = value.trim().to_ascii_lowercase();
            (!trimmed.is_empty()).then_some(trimmed)
        });

        let has_custom_base = base_url.as_ref().is_some_and(|v| !v.trim().is_empty());
        let mut config = OpenAiConfig::new_with_http_policy(
            api_key,
            if provider.trim() == "chatgpt_subscription" {
                None
            } else {
                base_url
            },
            default_model,
            getter(ALLOW_INSECURE_HTTP_ENV),
        );
        if let Some(timeout) = timeout {
            config = config.with_timeout(timeout);
        }
        if let Some(reasoning_effort) = reasoning_effort {
            config = config.with_reasoning_effort(Some(reasoning_effort));
        }
        config.auth_dir = auth_dir;
        match provider.trim() {
            "api_key" | "" => {}
            "chatgpt_subscription" => {
                config.provider = AiProvider::ChatGptSubscription;
                config.api_key = None;
                config.base_url = DEFAULT_BASE_URL.trim_end_matches('/').into();
                config.default_model = subscription_model.unwrap_or_default();
                if has_custom_base {
                    config.configuration_error = Some("ChatGPT subscription does not support custom base URLs. Unset AI_CHAT_BASE_URL and OPENAI_BASE_URL.".into());
                }
            }
            _ => {
                config.configuration_error =
                    Some("AI_CHAT_PROVIDER must be api_key or chatgpt_subscription.".into())
            }
        }
        config
    }

    pub fn provider_name(&self) -> &'static str {
        match self.provider {
            AiProvider::ApiKey => "api_key",
            AiProvider::ChatGptSubscription => "chatgpt_subscription",
        }
    }
    pub fn provider(&self) -> AiProvider {
        self.provider
    }
    pub fn auth_dir(&self) -> anyhow::Result<&Path> {
        self.auth_dir.as_deref().ok_or_else(|| anyhow::anyhow!("Subscription authentication needs HOME or XDG_CONFIG_HOME in the shell environment."))
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(error) = &self.configuration_error {
            anyhow::bail!("{error}");
        }
        Ok(())
    }
    pub fn readiness(&self) -> anyhow::Result<()> {
        self.validate()?;
        match self.provider {
            AiProvider::ApiKey => {
                if self.api_key.is_none() {
                    anyhow::bail!(
                        "OpenAI-compatible API key is not configured. {}",
                        API_KEY_SETUP_HINT
                    );
                }
            }
            AiProvider::ChatGptSubscription => {
                if self.default_model.is_empty() {
                    anyhow::bail!("Select AI_CHAT_SUBSCRIPTION_MODEL after chat_auth models.");
                }
                crate::auth::AuthStore::new(self.auth_dir()?.to_path_buf()).status()?;
            }
        }
        Ok(())
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

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }
}

fn sanitize_base_url(base_url: Option<String>, allow_http: Option<String>) -> String {
    let allow_insecure_http = allow_http
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);

    let sanitized = base_url
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.trim_end_matches('/').to_string())
            }
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.trim_end_matches('/').to_string());

    if allow_insecure_http || sanitized.starts_with("https://") {
        return sanitized;
    }

    let fallback = DEFAULT_BASE_URL.trim_end_matches('/').to_string();
    // Replacing the configured endpoint in silence is how a local `http://`
    // server turns into requests against api.openai.com: the key is accepted,
    // the answers come back, and nothing says the traffic left the machine.
    if sanitized != fallback {
        warn_insecure_base_url_replaced(&sanitized);
    }
    fallback
}

/// Announce the replacement once per process, on stderr and in the log.
///
/// This runs on every config load - once per agent turn - so repeating it would
/// bury the shell's own output.
fn warn_insecure_base_url_replaced(configured: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);

    tracing::warn!(
        configured_base_url = %configured,
        "base URL is not https; falling back to {DEFAULT_BASE_URL}"
    );

    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }

    eprintln!(
        "dsh: AI base URL `{configured}` is not https, so requests go to {DEFAULT_BASE_URL} instead. \
         Set {ALLOW_INSECURE_HTTP_ENV}=1 to use it as configured."
    );
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_blank_api_key_is_no_key() {
        let cfg = OpenAiConfig::new(Some("   ".to_string()), None, None);
        assert!(cfg.api_key().is_none());
    }

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

    #[test]
    fn from_getter_reads_the_reasoning_effort() {
        let getter = |key: &str| match key {
            "AI_CHAT_REASONING_EFFORT" => Some("  Low  ".to_string()),
            _ => None,
        };

        let cfg = OpenAiConfig::from_getter(getter);

        assert_eq!(cfg.reasoning_effort(), Some("low"));
    }

    #[test]
    fn from_getter_treats_a_blank_reasoning_effort_as_unset() {
        let getter = |key: &str| match key {
            "AI_CHAT_REASONING_EFFORT" => Some("   ".to_string()),
            _ => None,
        };

        let cfg = OpenAiConfig::from_getter(getter);

        assert_eq!(cfg.reasoning_effort(), None);
    }

    #[test]
    fn api_key_guidance_lists_every_supported_key_in_resolution_order() {
        let positions = API_KEY_ENV_VARS.map(|key| {
            API_KEY_SETUP_HINT
                .find(key)
                .unwrap_or_else(|| panic!("setup hint omitted {key}"))
        });

        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(v) = &self.previous {
                unsafe {
                    std::env::set_var(self.key, v);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn from_getter_reads_and_clamps_the_timeout() {
        let cfg = OpenAiConfig::from_getter(|key| match key {
            TIMEOUT_ENV => Some(" 300 ".to_string()),
            _ => None,
        });
        assert_eq!(cfg.timeout(), Duration::from_secs(300));

        let clamped = OpenAiConfig::from_getter(|key| match key {
            TIMEOUT_ENV => Some("999999".to_string()),
            _ => None,
        });
        assert_eq!(clamped.timeout(), Duration::from_secs(MAX_TIMEOUT_SECS));

        let default = OpenAiConfig::from_getter(|_| None);
        assert_eq!(default.timeout(), Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn insecure_http_policy_comes_from_the_supplied_getter() {
        let cfg = OpenAiConfig::from_getter(|key| match key {
            "AI_CHAT_BASE_URL" => Some("http://127.0.0.1:8888/v1".into()),
            ALLOW_INSECURE_HTTP_ENV => Some("1".into()),
            _ => None,
        });
        assert_eq!(cfg.base_url(), "http://127.0.0.1:8888/v1");
    }

    #[test]
    fn sanitize_base_url_rejects_insecure_http_by_default() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ALLOW_INSECURE_HTTP_ENV, "false");
        let cfg = OpenAiConfig::new(None, Some("http://localhost:8080/v1".to_string()), None);
        assert_eq!(cfg.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn sanitize_base_url_allows_http_with_explicit_opt_in() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ALLOW_INSECURE_HTTP_ENV, "true");
        let cfg = OpenAiConfig::new(None, Some("http://localhost:8080/v1".to_string()), None);
        assert_eq!(cfg.base_url(), "http://localhost:8080/v1");
    }
}

impl std::fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiConfig")
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;
    fn config(values: &[(&str, &str)]) -> OpenAiConfig {
        OpenAiConfig::from_getter(|key| {
            values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        })
    }
    #[test]
    fn provider_is_explicit_and_subscription_never_uses_api_credentials_or_model() {
        let api = config(&[("AI_CHAT_API_KEY", "key")]);
        assert_eq!(api.provider(), AiProvider::ApiKey);
        assert_eq!(api.default_model(), DEFAULT_MODEL);
        let sub = config(&[
            (PROVIDER_ENV, "chatgpt_subscription"),
            ("AI_CHAT_API_KEY", "key"),
            ("AI_CHAT_MODEL", "api-model"),
            ("HOME", "/tmp/mock-home"),
        ]);
        assert_eq!(sub.api_key(), None);
        assert_eq!(sub.default_model(), "");
        assert!(
            sub.readiness()
                .unwrap_err()
                .to_string()
                .contains("SUBSCRIPTION_MODEL")
        );
        assert_eq!(
            sub.auth_dir().unwrap(),
            Path::new("/tmp/mock-home/.config/dogesh/subscription-auth")
        );
        let sub = config(&[
            (PROVIDER_ENV, "chatgpt_subscription"),
            (SUBSCRIPTION_MODEL_ENV, "subscription-model"),
            ("AI_CHAT_BASE_URL", "https://other.example"),
        ]);
        assert!(sub.validate().is_err());
        assert_eq!(sub.api_key(), None);
        assert!(
            config(&[(PROVIDER_ENV, "typo"), ("AI_CHAT_API_KEY", "key")])
                .validate()
                .is_err()
        );
        assert!(!format!("{:?}", api).contains("\"key\""));
    }
    #[test]
    fn missing_getter_values_are_final_and_no_auth_path_is_invented() {
        let missing = OpenAiConfig::from_getter(|_| None);
        assert_eq!(missing.api_key(), None);
        assert!(missing.auth_dir().is_err());
        let config = config(&[
            (PROVIDER_ENV, "chatgpt_subscription"),
            (SUBSCRIPTION_MODEL_ENV, "model"),
            ("XDG_CONFIG_HOME", "/tmp/custom"),
        ]);
        assert_eq!(
            config.auth_dir().unwrap(),
            Path::new("/tmp/custom/dogesh/subscription-auth")
        );
    }
}
