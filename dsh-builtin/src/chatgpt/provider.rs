//! Explicit AI transport selection through the canonical configuration loader.
use super::{Context, ExitStatus, OpenAiConfig, ShellProxy, load_openai_config};
use dsh_openai::{AiProvider, PROVIDER_ENV};

pub fn chat_provider(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.get(1).is_some_and(|v| v == "models") {
        let selected = argv
            .get(2)
            .map(|v| AiProvider::parse(v))
            .unwrap_or_else(|| Ok(load_openai_config(proxy).provider()));
        return match selected {
            Ok(provider) if argv.len() <= 3 && provider.is_opencode() => {
                ctx.write_stdout(&format!("{} verified Chat Completions catalog (2026-10-15; availability depends on Console):", provider.name())).ok();
                for model in provider.opencode_models() {
                    ctx.write_stdout(model).ok();
                }
                ExitStatus::ExitedWith(0)
            }
            _ => {
                ctx.write_stderr("Usage: chat_provider models opencode_go|opencode_zen")
                    .ok();
                ExitStatus::ExitedWith(1)
            }
        };
    }
    match select(&argv, proxy) {
        Ok(provider) => {
            ctx.write_stdout(&format!("AI provider: {}", provider.name()))
                .ok();
            if provider.is_opencode() {
                ctx.write_stdout(&route_report(&load_openai_config(proxy)))
                    .ok();
            }
            if argv.len() >= 2 {
                ctx.write_stdout(
                    "Run chat_reset before continuing a conversation from another provider.",
                )
                .ok();
                if provider == AiProvider::ChatGptSubscription {
                    ctx.write_stdout("Use chat_auth login, chat_auth models, then chat_model <catalog-slug>. Subscription has no default model.").ok();
                }
            }
            ExitStatus::ExitedWith(0)
        }
        Err(error) => {
            ctx.write_stderr(&format!("chat_provider: {error}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn select(argv: &[String], proxy: &mut dyn ShellProxy) -> anyhow::Result<AiProvider> {
    match argv {
        [_] => {
            let config = load_openai_config(proxy);
            config.validate()?;
            Ok(config.provider())
        }
        [_, name] | [_, name, _] if !name.trim().is_empty() => {
            let provider = AiProvider::parse(name)?;
            let model = argv.get(2).map(|v| v.trim());
            if model.is_some() && !provider.is_opencode() {
                anyhow::bail!("An optional model is supported only for opencode_go/opencode_zen.");
            }
            let model_key = provider.opencode_model_env();
            let candidate = OpenAiConfig::from_getter(|key| {
                if key == PROVIDER_ENV {
                    Some(provider.name().into())
                } else if Some(key) == model_key && model.is_some() {
                    model.map(str::to_owned)
                } else {
                    proxy.get_var(key)
                }
            });
            candidate.validate()?;
            if let (Some(key), Some(model)) = (model_key, model) {
                proxy.set_var(key.into(), model.into());
            }
            proxy.set_var(PROVIDER_ENV.into(), provider.name().into());
            Ok(provider)
        }
        _ => anyhow::bail!(
            "Usage: chat_provider [api_key|chatgpt_subscription|chatgpt|opencode_go [model]|opencode_zen [model]]"
        ),
    }
}

/// Routing diagnostics contain key reference names and presence only.
pub(super) fn route_report(config: &OpenAiConfig) -> String {
    let provider = config.provider();
    let key = provider.opencode_key_env().unwrap_or("AI_CHAT_API_KEY");
    format!(
        "{}: {} (chat/completions), model={}\nKey references: {} or OPENCODE_API_KEY ({})\n{}",
        provider.name(),
        config.base_url(),
        config.default_model(),
        key,
        if config.api_key().is_some() {
            "configured"
        } else {
            "missing"
        },
        provider.opencode_billing().unwrap_or("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    #[test]
    fn selecting_alias_preserves_separate_model_and_key_settings() {
        let mut proxy = TestShellProxy::default();
        proxy
            .vars
            .insert("AI_CHAT_MODEL".into(), "api-model".into());
        proxy
            .vars
            .insert("AI_CHAT_API_KEY".into(), "mock-key".into());
        proxy.vars.insert(
            dsh_openai::SUBSCRIPTION_MODEL_ENV.into(),
            "catalog-slug".into(),
        );
        assert_eq!(
            select(&["chat_provider".into(), "chatgpt".into()], &mut proxy).unwrap(),
            AiProvider::ChatGptSubscription
        );
        assert_eq!(proxy.vars[PROVIDER_ENV], "chatgpt_subscription");
        let config = load_openai_config(&mut proxy);
        assert_eq!(config.default_model(), "catalog-slug");
        assert!(config.api_key().is_none());
        select(&["chat_provider".into(), "api_key".into()], &mut proxy).unwrap();
        let config = load_openai_config(&mut proxy);
        assert_eq!(config.default_model(), "api-model");
        assert_eq!(config.api_key(), Some("mock-key"));
    }

    #[test]
    fn invalid_selection_does_not_mutate_settings() {
        let mut proxy = TestShellProxy::default();
        proxy.vars.insert(PROVIDER_ENV.into(), "api_key".into());
        proxy.vars.insert(
            "AI_CHAT_BASE_URL".into(),
            "https://custom.example/v1".into(),
        );
        let before = proxy.vars.clone();
        for argv in [
            vec!["chat_provider", "chatgpt"],
            vec!["chat_provider", "openai"],
            vec!["chat_provider", ""],
            vec!["chat_provider", "api_key", "extra"],
        ] {
            assert!(
                select(
                    &argv.into_iter().map(String::from).collect::<Vec<_>>(),
                    &mut proxy
                )
                .is_err()
            );
            assert_eq!(proxy.vars, before);
        }
    }

    #[test]
    fn model_command_tracks_the_explicit_provider_without_restarting() {
        let mut proxy = TestShellProxy::default();
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);
        select(&["chat_provider".into(), "chatgpt".into()], &mut proxy).unwrap();
        assert_eq!(
            super::super::chat_model(
                &ctx,
                vec!["chat_model".into(), "catalog-model".into()],
                &mut proxy
            ),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(
            proxy.vars[dsh_openai::SUBSCRIPTION_MODEL_ENV],
            "catalog-model"
        );
        assert!(!proxy.vars.contains_key("AI_CHAT_MODEL"));
        select(&["chat_provider".into(), "api_key".into()], &mut proxy).unwrap();
        assert_eq!(
            super::super::chat_model(
                &ctx,
                vec!["chat_model".into(), "api-model".into()],
                &mut proxy
            ),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(proxy.vars["AI_CHAT_MODEL"], "api-model");
        assert_eq!(
            proxy.vars[dsh_openai::SUBSCRIPTION_MODEL_ENV],
            "catalog-model"
        );
    }
    #[test]
    fn opencode_selection_validates_before_mutation_and_remembers_each_model() {
        let mut proxy = TestShellProxy::default();
        proxy
            .vars
            .insert("AI_CHAT_API_KEY".into(), "generic-key".into());
        proxy
            .vars
            .insert("AI_CHAT_BASE_URL".into(), "https://generic.example".into());
        let before = proxy.vars.clone();
        for model in ["", "qwen3.8-max", "unknown"] {
            assert!(
                select(
                    &["chat_provider".into(), "opencode_go".into(), model.into()],
                    &mut proxy
                )
                .is_err()
            );
            assert_eq!(proxy.vars, before);
        }
        select(
            &[
                "chat_provider".into(),
                "opencode_go".into(),
                "glm-5.3".into(),
            ],
            &mut proxy,
        )
        .unwrap();
        select(
            &[
                "chat_provider".into(),
                "opencode_zen".into(),
                "qwen3.8-max".into(),
            ],
            &mut proxy,
        )
        .unwrap();
        select(&["chat_provider".into(), "opencode_go".into()], &mut proxy).unwrap();
        assert_eq!(load_openai_config(&mut proxy).default_model(), "glm-5.3");
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);
        let before = proxy.vars.clone();
        assert_eq!(
            super::super::chat_model(
                &ctx,
                vec!["chat_model".into(), "qwen3.8-max".into()],
                &mut proxy
            ),
            ExitStatus::ExitedWith(1)
        );
        assert_eq!(proxy.vars, before);
        assert_eq!(
            super::super::chat_model(
                &ctx,
                vec!["chat_model".into(), "glm-5.2".into()],
                &mut proxy
            ),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(load_openai_config(&mut proxy).default_model(), "glm-5.2");
        assert_eq!(proxy.vars["AI_CHAT_BASE_URL"], "https://generic.example");
        assert_eq!(proxy.vars["AI_CHAT_API_KEY"], "generic-key");
        proxy
            .vars
            .insert("OPENCODE_GO_API_KEY".into(), "full-secret-key".into());
        let report = route_report(&load_openai_config(&mut proxy));
        assert!(!report.contains("full-secret-key"));
        assert!(report.contains("Go subscription quota"));
    }
}
