use super::*;
use crate::auth::tests::{mock_http, seeded_store};

#[tokio::test]
async fn subscription_alias_catalog_selection_and_inference_use_oauth() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = seeded_store(dir.path().join("auth"));
    let (endpoint, requests, task) = mock_http(vec![
        (
            200,
            "application/json",
            json!({"models":[
                {"slug":"catalog-first","display_name":"First","visibility":"list"},
                {"slug":"hidden","display_name":"Hidden","visibility":"hide"},
                {"slug":"catalog-second","display_name":"Second","visibility":"list"}
            ]})
            .to_string(),
        ),
        (
            200,
            "text/event-stream",
            event(
                "response.completed",
                "response",
                completed(vec![message("subscription answer")]),
            ),
        ),
    ])
    .await;
    let config = crate::OpenAiConfig::from_getter(|key| match key {
        crate::PROVIDER_ENV => Some("chatgpt".into()),
        "OPENAI_API_KEY" => Some("mock-api-must-not-use".into()),
        _ => None,
    });
    config.validate().unwrap();
    let mut transport =
        SubscriptionTransport::new(store, config.default_model().into(), Duration::from_secs(2))
            .unwrap();
    let models = transport.models_at(&endpoint, None).await.unwrap();
    assert_eq!(
        models,
        vec![
            json!({"slug":"catalog-first","display_name":"First"}),
            json!({"slug":"catalog-second","display_name":"Second"})
        ]
    );
    transport.model = models[0]["slug"].as_str().unwrap().into();
    transport.endpoint = endpoint;
    let result = transport
        .send(
            &[json!({"role":"user","content":"hello"})],
            &ChatRequestOptions::new(),
            None,
            &mut |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        crate::turn::answer_text(&result).unwrap(),
        "subscription answer"
    );
    task.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer mock-access")
        );
        assert!(!request.contains("mock-api-must-not-use"));
    }
    assert!(requests[1].contains("\"model\":\"catalog-first\""));
    assert!(requests[1].contains("\"store\":false"));
    assert!(requests[1].contains("\"stream\":true"));
}
fn account() -> Account {
    Account {
        label: "mock-label".into(),
        client_id: "mock-client".into(),
        subject: "mock-subject".into(),
    }
}
fn options() -> ChatRequestOptions {
    ChatRequestOptions::new().with_tools(Some(vec![json!({"type":"function","function":{"name":"search","parameters":{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"],"additionalProperties":false}}})]))
}
fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}
fn completed(output: Vec<Value>) -> Value {
    json!({"status":"completed","output":output})
}
fn event(kind: &str, field: &str, value: Value) -> String {
    format!("data: {}\n\n", json!({"type":kind,field:value}))
}
#[test]
fn native_history_replays_order_and_encrypted_reasoning_after_serialization() {
    let account = account();
    let opts = options();
    let output = vec![
        json!({"type":"reasoning","id":"r1","encrypted_content":"opaque-do-not-display"}),
        json!({"type":"function_call","namespace":"doge_shell","name":"search","call_id":"exact-call-id","arguments":"{\"query\":\"x\"}"}),
        message("interim"),
    ];
    let response = normalize(&completed(output.clone()), &opts, "model", &account).unwrap();
    let assistant = crate::turn::interpret_response(&response)
        .unwrap()
        .assistant_message
        .unwrap();
    let serialized = serde_json::to_string(&assistant).unwrap();
    let assistant = serde_json::from_str(&serialized).unwrap();
    let body = build_body(
        &[
            json!({"role":"system","content":"instructions"}),
            json!({"role":"user","content":"find"}),
            assistant,
            json!({"role":"tool","tool_call_id":"exact-call-id","content":"{\"matches\":1}"}),
        ],
        &opts,
        "model",
        &account,
    )
    .unwrap();
    assert_eq!(body["input"][0]["role"], "developer");
    for (i, item) in output.iter().enumerate() {
        assert_eq!(body["input"][i + 2], *item);
    }
    assert_eq!(
        body["input"][5],
        json!({"type":"function_call_output","call_id":"exact-call-id","output":"{\"matches\":1}"})
    );
    assert_eq!(body["tools"][0]["tools"][0]["strict"], false);
    assert_eq!(
        body["tools"][0]["tools"][0]["parameters"],
        opts.tools.as_ref().unwrap()[0]["function"]["parameters"]
    );
    let view = crate::turn::interpret_response(&response).unwrap();
    assert_eq!(view.interim_text.as_deref(), Some("interim"));
    assert!(!format!("{:?}", view.outcome).contains("opaque-do-not-display"));
    assert!(
        build_body(
            &[response["choices"][0]["message"].clone()],
            &opts,
            "other-model",
            &account
        )
        .is_err()
    );
    let mut other = account.clone();
    other.client_id = "other-client".into();
    assert!(
        build_body(
            &[response["choices"][0]["message"].clone()],
            &opts,
            "model",
            &other
        )
        .is_err()
    );
    assert!(
        build_body(
            &[json!({"role":"assistant","content":"legacy"})],
            &opts,
            "model",
            &account
        )
        .is_err()
    );
}
#[test]
fn function_calls_allow_omission_but_refuse_unknown_namespace_duplicate_ids_and_invalid_json() {
    let call = json!({"type":"function_call","namespace":"doge_shell","name":"search","call_id":"c","arguments":"{\"query\":\"x\"}"});
    for arguments in ["{\"query\":\"x\"}", "{\"query\":\"x\",\"limit\":5}"] {
        let mut valid = call.clone();
        valid["arguments"] = json!(arguments);
        assert!(normalize(&completed(vec![valid]), &options(), "model", &account()).is_ok());
    }
    for (key, value) in [
        ("namespace", json!("other")),
        ("name", json!("unregistered")),
        ("call_id", json!("")),
        ("arguments", json!("not json")),
        ("arguments", json!("{}")),
        ("arguments", json!("[]")),
        ("arguments", json!("{\"query\":\"x\",\"limit\":null}")),
        ("arguments", json!("{\"query\":10}")),
        ("arguments", json!("{\"query\":\"x\",\"extra\":true}")),
    ] {
        let mut invalid = call.clone();
        invalid[key] = value;
        assert!(normalize(&completed(vec![invalid]), &options(), "model", &account()).is_err());
    }
    assert!(
        normalize(
            &completed(vec![call.clone(), call]),
            &options(),
            "model",
            &account()
        )
        .is_err()
    );
    assert!(
        normalize(
            &completed(vec![json!({"type":"tool_search_call"})]),
            &options(),
            "model",
            &account()
        )
        .is_err()
    );
}
#[test]
fn unsupported_fields_stay_absent_and_structured_output_is_preserved() {
    let opts=ChatRequestOptions::new().with_temperature(Some(0.1)).with_max_tokens(Some(1)).with_prompt_cache_key(Some("cache".into())).with_response_format(Some(json!({"type":"json_schema","json_schema":{"name":"answer","strict":true,"schema":{"type":"object","properties":{},"additionalProperties":false},"description":"description"}})));
    let body = build_body(
        &[json!({"role":"user","content":"JSON please"})],
        &opts,
        "model",
        &account(),
    )
    .unwrap();
    for field in [
        "temperature",
        "top_p",
        "max_tokens",
        "max_output_tokens",
        "max_completion_tokens",
        "background",
        "conversation",
        "metadata",
        "truncation",
        "previous_response_id",
        "prompt_cache_key",
    ] {
        assert!(body.get(field).is_none(), "{field}");
    }
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["text"]["format"]["name"], "answer");
    assert_eq!(body["text"]["format"]["strict"], true);
    assert!(
        normalize(
            &completed(vec![message("{\"ok\":true}")]),
            &opts,
            "model",
            &account()
        )
        .is_ok()
    );
    assert!(
        normalize(
            &completed(vec![message("partial JSON")]),
            &opts,
            "model",
            &account()
        )
        .is_err()
    );
    let refusal = json!({"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"reason"}]});
    assert!(
        normalize(&completed(vec![refusal]), &opts, "model", &account())
            .unwrap_err()
            .to_string()
            .contains("refused")
    );
    let json_object = opts.with_response_format(Some(json!({"type":"json_object"})));
    let body = build_body(&[], &json_object, "model", &account()).unwrap();
    assert_eq!(body["text"]["format"]["type"], "json_object");
    assert!(
        body["input"][0]["content"]
            .as_str()
            .unwrap()
            .contains("JSON")
    );
    let unsupported = json_object.with_response_format(Some(json!({"type":"unknown"})));
    assert!(build_body(&[], &unsupported, "model", &account()).is_err());
}
#[test]
fn usage_maps_cached_tokens_and_missing_usage_remains_unknown() {
    let mut native = completed(vec![message("ok")]);
    native["usage"] =
        json!({"input_tokens":12,"output_tokens":3,"input_tokens_details":{"cached_tokens":5}});
    let mapped = normalize(&native, &ChatRequestOptions::new(), "model", &account()).unwrap();
    let usage = crate::usage::TokenUsage::from_response(&mapped).unwrap();
    assert_eq!(usage.prompt_tokens, 12);
    assert_eq!(usage.completion_tokens, 3);
    assert_eq!(usage.cached_prompt_tokens, 5);
    native.as_object_mut().unwrap().remove("usage");
    assert!(
        crate::usage::TokenUsage::from_response(
            &normalize(&native, &ChatRequestOptions::new(), "model", &account()).unwrap()
        )
        .is_none()
    );
}
#[tokio::test]
async fn sse_multiple_deltas_complete_and_wire_contract() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = seeded_store(dir.path().join("subscription-auth"));
    let sse = event("response.output_text.delta", "delta", json!("Hel"))
        + &event("response.output_text.delta", "delta", json!("lo"))
        + &event(
            "response.completed",
            "response",
            completed(vec![message("Hello")]),
        );
    let (endpoint, requests, task) = mock_http(vec![(200, "text/event-stream", sse)]).await;
    let mut transport =
        SubscriptionTransport::new(store, "model".into(), Duration::from_secs(3)).unwrap();
    transport.endpoint = endpoint;
    let mut visible = String::new();
    let result = transport
        .send(
            &[json!({"role":"user","content":"hello"})],
            &ChatRequestOptions::new(),
            None,
            &mut |delta| visible.push_str(delta),
        )
        .await
        .unwrap();
    assert_eq!(visible, "Hello");
    assert_eq!(crate::turn::answer_text(&result).unwrap(), "Hello");
    task.await.unwrap();
    let request = requests.lock().unwrap();
    assert!(
        request[0]
            .to_lowercase()
            .contains("authorization: bearer mock-access")
    );
    let body: Value = serde_json::from_str(request[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
}
#[tokio::test]
async fn partial_failure_eof_incomplete_and_quota_never_retry() {
    for terminal in [
        event(
            "response.failed",
            "response",
            json!({"error":{"code":"server_error"}}),
        ),
        event("response.incomplete", "response", json!({})),
        String::new(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = seeded_store(dir.path().join("subscription-auth"));
        let (endpoint, requests, task) = mock_http(vec![(
            200,
            "text/event-stream",
            event("response.output_text.delta", "delta", json!("partial")) + &terminal,
        )])
        .await;
        let mut transport =
            SubscriptionTransport::new(store, "model".into(), Duration::from_secs(2)).unwrap();
        transport.endpoint = endpoint;
        let before = crate::usage::session_total();
        let mut visible = String::new();
        assert!(
            transport
                .send(&[], &ChatRequestOptions::new(), None, &mut |delta| visible
                    .push_str(delta))
                .await
                .is_err()
        );
        assert_eq!(visible, "partial");
        assert!(
            crate::usage::session_total()
                .since(&before)
                .unknown_usage_requests
                >= 1
        );
        task.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = seeded_store(dir.path().join("subscription-auth"));
    let (endpoint, requests, task) = mock_http(vec![(
        429,
        "application/json",
        r#"{"error":{"code":"usage_limit","message":"mock-secret"}}"#.into(),
    )])
    .await;
    let mut transport =
        SubscriptionTransport::new(store, "model".into(), Duration::from_secs(2)).unwrap();
    transport.endpoint = endpoint;
    let error = transport
        .send(&[], &ChatRequestOptions::new(), None, &mut |_| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("chatgpt.com/settings/usage"));
    assert!(!error.contains("mock-secret"));
    task.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn transient_before_visible_retries_and_cancellation_is_typed() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = seeded_store(dir.path().join("subscription-auth"));
    let (endpoint, requests, task) = mock_http(vec![
        (
            503,
            "application/json",
            r#"{"error":{"code":"server_error"}}"#.into(),
        ),
        (
            200,
            "text/event-stream",
            event(
                "response.completed",
                "response",
                completed(vec![message("ok")]),
            ),
        ),
    ])
    .await;
    let mut transport =
        SubscriptionTransport::new(store, "model".into(), Duration::from_secs(3)).unwrap();
    transport.endpoint = endpoint;
    assert!(
        transport
            .send(&[], &ChatRequestOptions::new(), None, &mut |_| {})
            .await
            .is_ok()
    );
    task.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
    let error = transport
        .send(&[], &ChatRequestOptions::new(), Some(&|| true), &mut |_| {})
        .await
        .unwrap_err();
    assert!(crate::is_ctrl_c_cancelled(&error));
}
