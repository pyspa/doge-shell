//! Base URL validation: an unusable or insecure URL is a configuration
//! error, never silently replaced with the OpenAI endpoint.
use super::*;

#[test]
fn a_base_url_without_a_scheme_fails_closed() {
    let cfg = OpenAiConfig::new_with_http_policy(
        Some("groq-key".into()),
        Some("api.groq.com/openai/v1".into()),
        None,
        None,
    );
    assert_ne!(cfg.base_url(), "https://api.openai.com/v1");
    assert!(cfg.validate().is_err());
    assert!(cfg.readiness().is_err());

    // The http opt-in is about transport security, not about guessing
    // a scheme: a scheme-less URL is still refused.
    let opted_in = OpenAiConfig::new_with_http_policy(
        Some("groq-key".into()),
        Some("api.groq.com/openai/v1".into()),
        None,
        Some("1".into()),
    );
    assert!(opted_in.validate().is_err());
}

#[test]
fn the_base_url_scheme_is_case_insensitive() {
    let cfg = OpenAiConfig::new_with_http_policy(
        Some("groq-key".into()),
        Some("HTTPS://api.groq.com/openai/v1/".into()),
        None,
        None,
    );
    assert!(cfg.validate().is_ok());
    assert_eq!(cfg.base_url(), "https://api.groq.com/openai/v1");

    let insecure = OpenAiConfig::new_with_http_policy(
        Some("local-key".into()),
        Some("HTTP://127.0.0.1:8080/v1".into()),
        None,
        None,
    );
    assert!(insecure.validate().is_err());

    let opted_in = OpenAiConfig::new_with_http_policy(
        Some("local-key".into()),
        Some("HTTP://127.0.0.1:8080/v1".into()),
        None,
        Some("1".into()),
    );
    assert!(opted_in.validate().is_ok());
    assert_eq!(opted_in.base_url(), "http://127.0.0.1:8080/v1");
}

#[test]
fn from_getter_refuses_an_insecure_base_url() {
    let cfg = OpenAiConfig::from_getter(|key| match key {
        "AI_CHAT_API_KEY" => Some("local-key".into()),
        "AI_CHAT_BASE_URL" => Some("http://127.0.0.1:8888/v1".into()),
        _ => None,
    });
    assert!(cfg.validate().is_err());
}

/// A blank `AI_CHAT_BASE_URL` used to win the lookup and then fall back
/// to api.openai.com, sending the key meant for `OPENAI_BASE_URL`'s
/// server to OpenAI.
#[test]
fn from_getter_skips_a_blank_primary_base_url() {
    for blank in ["", "   "] {
        let cfg = OpenAiConfig::from_getter(|key| match key {
            "AI_CHAT_BASE_URL" => Some(blank.to_string()),
            "OPENAI_BASE_URL" => Some("https://api.groq.com/openai/v1".to_string()),
            "OPENAI_API_KEY" => Some("groq-key".to_string()),
            _ => None,
        });
        assert_eq!(
            cfg.base_url(),
            "https://api.groq.com/openai/v1",
            "{blank:?}"
        );
    }
}
