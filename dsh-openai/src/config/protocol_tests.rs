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
fn responses_is_the_default_and_chat_completions_is_opt_in() {
    let default = config(&[("AI_CHAT_API_KEY", "key")]);
    assert_eq!(default.api_protocol(), ApiProtocol::Responses);
    assert_eq!(
        default.responses_endpoint(),
        "https://api.openai.com/v1/responses"
    );
    let local = config(&[
        ("AI_CHAT_API_KEY", "key"),
        ("AI_CHAT_BASE_URL", "https://local.example/v1/"),
    ]);
    assert_eq!(local.api_protocol(), ApiProtocol::Responses);
    assert_eq!(
        local.responses_endpoint(),
        "https://local.example/v1/responses"
    );
    for value in ["chat_completions", "chat-completions", "compat"] {
        let compat = config(&[("AI_CHAT_API_KEY", "key"), (API_PROTOCOL_ENV, value)]);
        assert!(compat.validate().is_ok(), "{value}");
        assert_eq!(compat.api_protocol(), ApiProtocol::ChatCompletions);
    }
    let blank = config(&[("AI_CHAT_API_KEY", "key"), (API_PROTOCOL_ENV, " ")]);
    assert_eq!(blank.api_protocol(), ApiProtocol::Responses);
    assert!(
        config(&[("AI_CHAT_API_KEY", "key"), (API_PROTOCOL_ENV, "chat")])
            .validate()
            .is_err()
    );
}
#[test]
fn a_chat_completions_base_url_needs_the_explicit_protocol() {
    let base = (
        "AI_CHAT_BASE_URL",
        "https://local.example/v1/chat/completions",
    );
    let error = config(&[("AI_CHAT_API_KEY", "key"), base])
        .validate()
        .unwrap_err()
        .to_string();
    assert!(error.contains(API_PROTOCOL_ENV), "{error}");
    assert!(
        config(&[
            ("AI_CHAT_API_KEY", "key"),
            base,
            (API_PROTOCOL_ENV, "chat_completions")
        ])
        .validate()
        .is_ok()
    );
    // Every constructor gets the check, not only the environment parser.
    let direct = OpenAiConfig::new(
        Some("key".into()),
        Some("https://local.example/v1/chat/completions".into()),
        None,
    );
    assert!(direct.validate().is_err());
    assert!(
        direct
            .with_api_protocol(ApiProtocol::ChatCompletions)
            .validate()
            .is_ok()
    );
}
#[test]
fn other_providers_keep_their_fixed_protocol() {
    let sub = config(&[
        (PROVIDER_ENV, "chatgpt_subscription"),
        (API_PROTOCOL_ENV, "chat_completions"),
    ]);
    assert_eq!(sub.api_protocol(), ApiProtocol::Responses);
    let go = config(&[
        (PROVIDER_ENV, "opencode_go"),
        (API_PROTOCOL_ENV, "responses"),
    ]);
    assert_eq!(go.api_protocol(), ApiProtocol::ChatCompletions);
}
