//! Explicit AI transport selection through the canonical configuration loader.
use super::{Context, ExitStatus, OpenAiConfig, ShellProxy, load_openai_config};
use dsh_openai::{AiProvider, PROVIDER_ENV};

pub fn chat_provider(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match select(&argv, proxy) {
        Ok(provider) => {
            ctx.write_stdout(&format!("AI provider: {}", provider.name()))
                .ok();
            if argv.len() == 2 {
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
        [_, name] if !name.trim().is_empty() => {
            let provider = AiProvider::parse(name)?;
            // Validate before mutation; never silently remove a custom API URL.
            let candidate = OpenAiConfig::from_getter(|key| {
                if key == PROVIDER_ENV {
                    Some(provider.name().into())
                } else {
                    proxy.get_var(key)
                }
            });
            candidate.validate()?;
            // set_var republishes provider/model to all shell AI consumers.
            proxy.set_var(PROVIDER_ENV.into(), provider.name().into());
            Ok(provider)
        }
        _ => anyhow::bail!("Usage: chat_provider [api_key|chatgpt_subscription|chatgpt]"),
    }
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
}
