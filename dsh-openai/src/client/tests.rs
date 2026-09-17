use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, sleep};

const TEST_API_KEY: &str = "test-key";
const TEST_BASE_URL: &str = "https://example.invalid";

fn client() -> ChatGptClient {
    ChatGptClient::new_with_settings(
        TEST_API_KEY.to_string(),
        Some("gpt-5-mini".to_string()),
        Some(TEST_BASE_URL.to_string()),
    )
    .expect("client should initialize")
}

/// Like [`client`], but with `AI_CHAT_REASONING_EFFORT` set. Goes through
/// `OpenAiConfig::from_getter` rather than `client()`'s `new_with_settings`
/// because only `from_getter` reads it (see `new_with_http_policy`'s
/// doc comment in `config.rs`) - and the getter is a closure, never real
/// process env, so parallel tests cannot race each other over it. Uses
/// `gpt-5.6-luna` rather than `client()`'s `gpt-5-mini` so the reported
/// model stays covered by at least one test independently of the shared
/// reasoning-lineup prefix match.
fn client_with_reasoning_effort(effort: &str) -> ChatGptClient {
    let effort = effort.to_string();
    let config = OpenAiConfig::from_getter(move |key| match key {
        "AI_CHAT_API_KEY" => Some(TEST_API_KEY.to_string()),
        "AI_CHAT_MODEL" => Some("gpt-5.6-luna".to_string()),
        "AI_CHAT_BASE_URL" => Some(TEST_BASE_URL.to_string()),
        "AI_CHAT_REASONING_EFFORT" => Some(effort.clone()),
        _ => None,
    });
    ChatGptClient::try_from_config(&config).expect("client should initialize")
}

#[tokio::test]
async fn test_await_with_cancel_normal_completion() {
    let future = async { Ok::<_, anyhow::Error>("success") };
    let result = ChatGptClient::await_with_cancel(future, None).await;
    assert_eq!(result.unwrap(), "success");
}

#[tokio::test]
async fn test_await_with_cancel_via_callback() {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = cancel_flag.clone();

    // Callback that returns the value of the flag
    let check = move || flag_clone.load(Ordering::SeqCst);

    // Future that waits long enough
    let future = async {
        sleep(Duration::from_secs(5)).await;
        Ok::<_, anyhow::Error>("should not be reached")
    };

    // Spawn a task to set the flag after 200ms
    let flag_clone2 = cancel_flag.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(200)).await;
        flag_clone2.store(true, Ordering::SeqCst);
    });

    let result = ChatGptClient::await_with_cancel(future, Some(&check)).await;

    assert!(result.is_err());
    assert!(is_ctrl_c_cancelled(&result.unwrap_err()));
}

/// The blocking entry point is called from inside the REPL's runtime, so
/// it must hand the worker over rather than start a nested one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_send_chat_inside_runtime_does_not_panic() {
    let client = client();
    let messages = vec![json!({ "role": "user", "content": "hello" })];
    let options = ChatRequestOptions::new().with_temperature(Some(0.0));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.send_chat(&messages, &options, Some(&|| true))
    }));

    assert!(result.is_ok(), "send_chat panicked inside runtime");
    assert!(result.expect("panic check").is_err());
}

#[test]
fn build_body_includes_optional_fields() {
    let client = client();
    let messages = vec![json!({ "role": "user", "content": "hi" })];
    let options = ChatRequestOptions::new()
        .with_temperature(Some(0.2))
        .with_model(Some("gpt-4.1-mini".to_string()))
        .with_max_tokens(Some(256))
        .with_response_format(Some(json!({ "type": "json_object" })))
        .with_prompt_cache_key(Some("dsh-agent".to_string()));

    let body = client.build_body(&messages, &options);

    assert_eq!(body["model"], "gpt-4.1-mini");
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["max_completion_tokens"], 256);
    assert_eq!(body["response_format"]["type"], "json_object");
    assert_eq!(body["prompt_cache_key"], "dsh-agent");
    assert!(body.get("tools").is_none());
}

#[test]
fn build_body_forces_default_temperature_for_gpt_5_mini() {
    let client = client();
    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_temperature(Some(0.0)),
    );

    assert_eq!(body["temperature"], 1.0);
}

/// The exact-match version of this check let every reasoning model other
/// than `gpt-5-mini` receive a temperature it answers with a 400. Shared
/// by `final_temperature` and `resolve_reasoning_effort`
/// (`is_openai_reasoning_model`), so this doubles as the family-matching
/// coverage for both.
#[test]
fn openai_reasoning_models_are_matched_by_family() {
    for model in [
        "gpt-5",
        "gpt-5-mini",
        "gpt-5.1-codex",
        "gpt-5.6-luna",
        "o1-preview",
        "o3",
        "o3-mini",
        "o4-mini",
        "openai/gpt-5-mini",
        "GPT-5-Mini",
    ] {
        assert!(
            is_openai_reasoning_model(model),
            "{model} should be recognised as a member of the reasoning lineup"
        );
    }

    for model in ["gpt-4.1-mini", "gpt-4o", "o1x-turbo", "gpt-51", "llama3"] {
        assert!(
            !is_openai_reasoning_model(model),
            "{model} should not be recognised as a member of the reasoning lineup"
        );
    }
}

/// An endpoint that rejects `temperature` must cost one retry, not the turn.
#[test]
fn build_body_drops_temperature_once_the_endpoint_rejects_it() {
    let client = client();
    client.remember_unsupported("temperature");

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new()
            .with_temperature(Some(0.2))
            .with_model(Some("gpt-4.1-mini".to_string())),
    );

    assert!(body.get("temperature").is_none());
}

#[test]
fn build_body_adds_stream_and_stream_options_when_requested() {
    let client = client();
    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_stream(true),
    );

    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[test]
fn build_body_omits_stream_fields_by_default() {
    let client = client();
    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new(),
    );

    assert!(body.get("stream").is_none());
    assert!(body.get("stream_options").is_none());
}

/// A server that answers `stream_options` with a 400 must keep streaming
/// the reply, just without usage - not fall all the way back to a single
/// non-streaming response.
#[test]
fn build_body_drops_only_stream_options_once_the_endpoint_rejects_it() {
    let client = client();
    client.remember_unsupported("stream_options");

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_stream(true),
    );

    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());
}

/// A server that rejects `stream` itself degrades the whole turn to a
/// single non-streaming response.
#[test]
fn build_body_drops_stream_once_the_endpoint_rejects_it() {
    let client = client();
    client.remember_unsupported("stream");

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_stream(true),
    );

    assert!(body.get("stream").is_none());
    // Dropping `stream` makes `stream_options` meaningless too, even
    // though it was never itself rejected.
    assert!(body.get("stream_options").is_none());
}

/// A 400 naming `stream` must degrade in one retry, not two: `stream_options`
/// without `stream` is meaningless, so `recover` drops both together.
/// Otherwise the retried body still carried `stream_options` and paid for a
/// second 400 before reaching the non-streaming shape.
#[test]
fn recover_drops_stream_options_alongside_stream() {
    let client = client();
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unrecognized request argument supplied: stream".into(),
    }
    .into();
    let mut body = json!({
        "model": "gpt-4.1-mini",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    let mut state = RecoveryState::seed(client.known_unsupported());

    assert!(client.recover(&err, &mut body, &mut state));

    assert!(body.get("stream").is_none());
    assert!(body.get("stream_options").is_none());
    assert!(state.dropped.contains(&"stream"));
    assert!(state.dropped.contains(&"stream_options"));
    // A rebuild from the learned memory reaches the same shape without
    // another round-trip.
    let rebuilt = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_stream(true),
    );
    assert!(rebuilt.get("stream").is_none());
    assert!(rebuilt.get("stream_options").is_none());
}

#[test]
fn build_body_sends_the_configured_reasoning_effort() {
    let client = client_with_reasoning_effort("high");

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new(),
    );

    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn build_body_omits_reasoning_effort_when_unset() {
    let client = client();

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new(),
    );

    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn build_body_drops_reasoning_effort_once_the_endpoint_rejects_the_field_itself() {
    let client = client_with_reasoning_effort("high");
    client.remember_unsupported("reasoning_effort");

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new(),
    );

    assert!(body.get("reasoning_effort").is_none());
}

/// A `tools`-less request (summarization, `safe_run`'s JSON generation)
/// telling us the endpoint has never heard of `reasoning_effort` at all
/// must not poison every other model sharing this client: the field is
/// dropped for this one retry, but not remembered client-wide, so a
/// later `tools` request to the model that actually needs the field can
/// still be corrected by `reasoning_effort_conflict`.
#[test]
fn a_non_tools_requests_unsupported_reasoning_effort_is_not_remembered_client_wide() {
    let client = client_with_reasoning_effort("high");
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unrecognized request argument supplied: reasoning_effort".into(),
    }
    .into();
    let mut body = json!({ "model": "gpt-5-mini", "reasoning_effort": "high" });
    let mut state = RecoveryState::seed(client.known_unsupported());

    assert!(client.recover(&err, &mut body, &mut state));

    assert!(
        body.get("reasoning_effort").is_none(),
        "dropped for this retry"
    );
    assert!(
        !client.known_unsupported().contains(&"reasoning_effort"),
        "must not be remembered client-wide from a tools-less request"
    );
}

/// The mirror case: a `tools` request itself telling us the endpoint
/// never heard of `reasoning_effort` at all is a genuine, endpoint-wide
/// incompatibility - remembering it client-wide is correct and safe
/// (unlike forcing `"none"`, dropping the field entirely is never wrong
/// for a server that rejects it outright).
#[test]
fn a_tools_requests_unsupported_reasoning_effort_is_remembered_client_wide() {
    let client = client_with_reasoning_effort("high");
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unrecognized request argument supplied: reasoning_effort".into(),
    }
    .into();
    let mut body = json!({
        "model": "gpt-5-mini",
        "reasoning_effort": "high",
        "tools": [{ "type": "function" }],
    });
    let mut state = RecoveryState::seed(client.known_unsupported());

    assert!(client.recover(&err, &mut body, &mut state));

    assert!(body.get("reasoning_effort").is_none());
    assert!(client.known_unsupported().contains(&"reasoning_effort"));
}

/// End-to-end regression for the cross-model poisoning this client-wide
/// memo used to allow: a `tools`-less request's rejection must not block
/// the *reactive* `tools`/`reasoning_effort` correction for a later
/// request on the same client.
#[test]
fn a_non_tools_rejection_does_not_block_a_later_tools_conflict_correction() {
    let client = client_with_reasoning_effort("high");
    let mut state = RecoveryState::seed(client.known_unsupported());

    let unsupported_err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unrecognized request argument supplied: reasoning_effort".into(),
    }
    .into();
    let mut summary_body = json!({ "model": "gpt-4o-mini", "reasoning_effort": "high" });
    assert!(client.recover(&unsupported_err, &mut summary_body, &mut state));

    // A fresh request/state, as a later `!` turn iteration would build -
    // only the client's persistent memory carries over.
    let mut state = RecoveryState::seed(client.known_unsupported());
    let mut tools_body = json!({
        "model": "gpt-5-mini",
        "reasoning_effort": "high",
        "tools": [{ "type": "function", "function": { "name": "execute" } }],
    });
    let conflict_err = gpt_5_6_luna_reasoning_conflict();

    assert!(
        client.recover(&conflict_err, &mut tools_body, &mut state),
        "the earlier tools-less rejection must not have disabled this correction"
    );
    assert_eq!(tools_body["reasoning_effort"], "none");
}

#[test]
fn contains_word_matches_whole_words_only() {
    assert!(contains_word(
        "function tools are not supported",
        "function"
    ));
    assert!(contains_word("reasoning_effort with tools", "tools"));
    assert!(!contains_word("dysfunctional output", "function"));
    assert!(!contains_word("toolchain misconfigured", "tool"));
}

/// Once a `tools` request has been corrected, every later `tools` request
/// on the same client must send `"none"` too - `build_body` runs on every
/// iteration of a `!` chat turn, so without this the shell would pay the
/// same 400 on every tool-using round instead of once per turn.
#[test]
fn build_body_forces_none_for_tools_requests_after_the_endpoint_asked_for_it() {
    let client = client_with_reasoning_effort("high");
    client.remember_reasoning_none_forced("gpt-5.6-luna");

    let tools = vec![json!({ "type": "function", "function": { "name": "execute" } })];
    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_tools(Some(tools)),
    );

    assert_eq!(body["reasoning_effort"], "none");
}

/// The forced `"none"` must not leak into requests that never offer
/// `tools` - summarization (`AI_SUMMARY_MODEL`) and JSON-generating
/// requests (`safe_run`, ghost text) still want the configured quality.
#[test]
fn build_body_keeps_the_configured_effort_without_tools_even_after_the_correction() {
    let client = client_with_reasoning_effort("high");
    client.remember_reasoning_none_forced("gpt-5.6-luna");

    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new(),
    );

    assert_eq!(body["reasoning_effort"], "high");
}

/// No `AI_CHAT_REASONING_EFFORT` at all, but the model is a known member
/// of the reasoning lineup and the request carries `tools`: `build_body`
/// must send `"none"` on the very first attempt rather than waiting for
/// the server to 400 and correcting reactively - the whole point of the
/// model-gated default is to make `gpt-5.6-luna` (and the shell's own
/// default `gpt-5-mini`) work with zero configuration and zero wasted
/// round-trips.
#[test]
fn build_body_defaults_reasoning_effort_to_none_for_tools_on_a_known_reasoning_model() {
    let client = client(); // model "gpt-5-mini", no reasoning_effort configured

    let tools = vec![json!({ "type": "function", "function": { "name": "execute" } })];
    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_tools(Some(tools)),
    );

    assert_eq!(body["reasoning_effort"], "none");
}

/// The same default must not reach a model outside the known reasoning
/// lineup - a local/compatible server or a plain chat model that has
/// never heard of `reasoning_effort` would otherwise pay one wasted 400
/// on every `!` message for a field nobody asked to send.
#[test]
fn build_body_does_not_default_reasoning_effort_for_an_unknown_model() {
    let client = client(); // no reasoning_effort configured
    let tools = vec![json!({ "type": "function", "function": { "name": "execute" } })];
    let body = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new()
            .with_model(Some("llama3".to_string()))
            .with_tools(Some(tools)),
    );

    assert!(body.get("reasoning_effort").is_none());
}

/// `"stream_options"` contains `"stream"` as a substring: an error
/// message naming the longer field must not be misread as naming the
/// shorter one, or a stream-capable server would be downgraded to a
/// single non-streaming response over a field it never rejected.
#[test]
fn unsupported_field_prefers_the_more_specific_stream_option_name() {
    let body = json!({ "stream": true, "stream_options": { "include_usage": true } });
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unrecognized request argument supplied: stream_options".into(),
    }
    .into();

    assert_eq!(unsupported_field(&err, &body, &[]), Some("stream_options"));
}

#[test]
fn unsupported_field_matches_stream_when_only_stream_is_named() {
    let body = json!({ "stream": true });
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "'stream' is not supported for this model".into(),
    }
    .into();

    assert_eq!(unsupported_field(&err, &body, &[]), Some("stream"));
}

fn gpt_5_6_luna_reasoning_conflict() -> Error {
    ApiError {
        status: Some(400),
        retry_after: None,
        message: "Function tools with reasoning_effort are not supported for gpt-5.6-luna \
                      in /v1/chat/completions. To use function tools, use /v1/responses or set \
                      reasoning_effort to 'none'."
            .into(),
    }
    .into()
}

#[test]
fn a_tools_request_rejected_over_reasoning_effort_is_corrected_to_none() {
    let err = gpt_5_6_luna_reasoning_conflict();
    let body = json!({
        "model": "gpt-5.6-luna",
        "tools": [{ "type": "function", "function": { "name": "execute" } }],
    });

    assert_eq!(
        reasoning_effort_conflict(&err, &body, &[]),
        Some("gpt-5.6-luna".to_string())
    );
}

/// An operator typo in `AI_CHAT_REASONING_EFFORT` produces a 400 that also
/// names the field (e.g. an enum-validation error), but says nothing
/// about tools or function calling. Misreading that as a `tools`
/// conflict would force `"none"` and latch it for the rest of the
/// client's life over what was actually a config mistake.
#[test]
fn a_reasoning_effort_error_without_tool_wording_is_not_a_tools_conflict() {
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Invalid value for reasoning_effort: must be one of low, medium, high, none"
            .into(),
    }
    .into();
    let body = json!({
        "model": "gpt-5.6-luna",
        "tools": [{ "type": "function", "function": { "name": "execute" } }],
    });

    assert_eq!(reasoning_effort_conflict(&err, &body, &[]), None);
}

#[test]
fn a_request_without_tools_is_not_corrected() {
    let err = gpt_5_6_luna_reasoning_conflict();
    let body = json!({ "model": "gpt-5.6-luna" });

    assert_eq!(reasoning_effort_conflict(&err, &body, &[]), None);
}

#[test]
fn a_body_that_already_says_none_is_not_corrected_again() {
    let err = gpt_5_6_luna_reasoning_conflict();
    let body = json!({
        "tools": [{ "type": "function" }],
        "reasoning_effort": "none",
    });

    assert_eq!(reasoning_effort_conflict(&err, &body, &[]), None);
}

#[test]
fn a_server_that_rejects_the_field_itself_stops_the_correction() {
    let err = gpt_5_6_luna_reasoning_conflict();
    let body = json!({ "tools": [{ "type": "function" }], "reasoning_effort": "high" });

    assert_eq!(
        reasoning_effort_conflict(&err, &body, &["reasoning_effort"]),
        None
    );
}

/// If `unsupported_field` ran before the additive correction, this 400
/// would have `reasoning_effort` *dropped* (it is in `DROPPABLE_FIELDS`
/// and the message names it) instead of forced to `"none"` - the server's
/// non-`none` default would stay in effect and the identical error would
/// come back with nothing left in `state` to try.
#[test]
fn the_reasoning_correction_precedes_dropping_the_field() {
    let client = client();
    let err = gpt_5_6_luna_reasoning_conflict();
    let mut body = json!({
        "model": "gpt-5.6-luna",
        "tools": [{ "type": "function", "function": { "name": "execute" } }],
    });
    let mut state = RecoveryState::seed(client.known_unsupported());

    assert!(client.recover(&err, &mut body, &mut state));

    assert_eq!(body["reasoning_effort"], "none");
    assert!(body.get("tools").is_some());
    assert!(!state.dropped.contains(&"reasoning_effort"));
    assert!(client.reasoning_none_forced());
}

/// A server that keeps answering the identical 400 must not recover
/// forever: once `reasoning_effort` has been forced to `"none"` and then
/// (a server that never learns the field at all) dropped outright,
/// `recover` must stop offering a body change so the caller's ordinary
/// 400-is-terminal handling in `send_with_retry` takes over.
#[test]
fn the_recovery_loop_is_bounded_for_a_repeating_reasoning_effort_conflict() {
    let client = client();
    let err = gpt_5_6_luna_reasoning_conflict();
    let mut body = json!({
        "model": "gpt-5.6-luna",
        "tools": [{ "type": "function", "function": { "name": "execute" } }],
    });
    let mut state = RecoveryState::seed(client.known_unsupported());

    assert!(client.recover(&err, &mut body, &mut state));
    assert_eq!(body["reasoning_effort"], "none");

    assert!(client.recover(&err, &mut body, &mut state));
    assert!(body.get("reasoning_effort").is_none());

    assert!(!client.recover(&err, &mut body, &mut state));
}

#[test]
fn retry_delay_retries_429_and_5xx_but_not_401() {
    let too_many: Error = ApiError {
        status: Some(429),
        retry_after: None,
        message: "slow down".into(),
    }
    .into();
    assert!(retry_delay(&too_many, 1).is_some());

    let server: Error = ApiError {
        status: Some(503),
        retry_after: None,
        message: "unavailable".into(),
    }
    .into();
    assert!(retry_delay(&server, 2).is_some());

    let unauthorized: Error = ApiError {
        status: Some(401),
        retry_after: None,
        message: "bad key".into(),
    }
    .into();
    assert!(retry_delay(&unauthorized, 1).is_none());
}

#[test]
fn retry_delay_gives_up_after_max_retries() {
    let err: Error = ApiError {
        status: Some(500),
        retry_after: None,
        message: "boom".into(),
    }
    .into();

    assert!(retry_delay(&err, MAX_RETRIES).is_some());
    assert!(retry_delay(&err, MAX_RETRIES + 1).is_none());
}

#[test]
fn retry_delay_honours_retry_after() {
    let err: Error = ApiError {
        status: Some(429),
        retry_after: Some(Duration::from_secs(5)),
        message: "slow down".into(),
    }
    .into();

    assert_eq!(retry_delay(&err, 1), Some(Duration::from_secs(5)));
}

#[test]
fn retry_delay_backoff_grows_and_is_capped() {
    let err: Error = ApiError {
        status: Some(500),
        retry_after: None,
        message: "boom".into(),
    }
    .into();

    let first = retry_delay(&err, 1).unwrap();
    let second = retry_delay(&err, 2).unwrap();
    assert!(second > first);
    assert!(retry_delay(&err, MAX_RETRIES).unwrap() <= MAX_RETRY_DELAY);
}

#[test]
fn unsupported_field_is_detected_and_only_dropped_once() {
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unsupported parameter: 'max_completion_tokens' is not supported".into(),
    }
    .into();
    let body = json!({ "model": "local", "max_completion_tokens": 32 });

    assert_eq!(
        unsupported_field(&err, &body, &[]),
        Some("max_completion_tokens")
    );
    assert_eq!(
        unsupported_field(&err, &body, &["max_completion_tokens"]),
        None
    );
}

#[test]
fn unsupported_field_ignores_non_400_errors() {
    let err: Error = ApiError {
        status: Some(500),
        retry_after: None,
        message: "max_completion_tokens".into(),
    }
    .into();
    let body = json!({ "max_completion_tokens": 32 });

    assert_eq!(unsupported_field(&err, &body, &[]), None);
}

#[test]
fn retry_delay_does_not_retry_a_timeout() {
    // A timeout already spent the full request budget; retrying multiplies
    // the worst case by MAX_RETRIES.
    let client = Client::builder()
        .timeout(Duration::from_millis(1))
        .build()
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = runtime.block_on(async {
        client
            .get("http://10.255.255.1/")
            .send()
            .await
            .expect_err("must time out")
    });

    assert!(err.is_timeout() || err.is_connect());
    if err.is_timeout() {
        assert!(retry_delay(&anyhow::Error::from(err), 1).is_none());
    }
}

#[test]
fn a_rejected_field_is_remembered_for_later_requests() {
    let client = client();
    let err: Error = ApiError {
        status: Some(400),
        retry_after: None,
        message: "Unsupported parameter: 'response_format'".into(),
    }
    .into();
    let body = json!({ "response_format": { "type": "json_object" } });

    assert_eq!(
        unsupported_field(&err, &body, &client.known_unsupported()),
        Some("response_format")
    );

    client.remember_unsupported("response_format");

    // The next request must not send it again.
    let rebuilt = client.build_body(
        &[json!({ "role": "user", "content": "hi" })],
        &ChatRequestOptions::new().with_response_format(Some(json!({ "type": "json_object" }))),
    );
    assert!(rebuilt.get("response_format").is_none());
    assert_eq!(
        unsupported_field(&err, &body, &client.known_unsupported()),
        None
    );
}

/// Read one HTTP request off `stream` and parse its JSON body. Mirrors
/// `dsh/src/agent/tests.rs`'s fixture, which stands in for a real
/// OpenAI-compatible server across the crate.
fn read_request(stream: &mut std::net::TcpStream) -> Value {
    use std::io::Read;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let header = String::from_utf8(bytes).unwrap();
    let size = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn reply(stream: &mut std::net::TcpStream, status_line: &str, body: &Value) {
    use std::io::Write;
    let text = body.to_string();
    write!(
            stream,
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        )
        .unwrap();
}

/// End-to-end: a real `send_chat` call against a fake server that answers
/// the exact 400 `gpt-5.6-luna` returns for `tools`, then accepts the
/// corrected retry - covering the full path (`build_body` ->
/// `send_with_retry` -> `recover` -> `build_body`'s forced-none branch)
/// that the unit tests above exercise piecewise.
/// An operator who explicitly overrode `AI_CHAT_REASONING_EFFORT` to a
/// value that conflicts with `tools` still gets the reactive correction:
/// the override is tried as configured first (it might not conflict on a
/// different endpoint), and only corrected once the server actually says
/// so. The model-gated proactive default below covers the common case of
/// no override at all, so this exercises the fallback path it does not.
#[test]
fn a_reasoning_effort_conflict_is_corrected_over_the_wire() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let first = read_request(&mut stream);
        assert!(first.get("tools").is_some());
        assert_eq!(first["reasoning_effort"], "medium");
        reply(
            &mut stream,
            "400 Bad Request",
            &json!({
                "error": {
                    "message": "Function tools with reasoning_effort are not supported \
                                for gpt-5.6-luna in /v1/chat/completions. To use function \
                                tools, use /v1/responses or set reasoning_effort to 'none'."
                }
            }),
        );

        let (mut stream, _) = listener.accept().unwrap();
        let second = read_request(&mut stream);
        assert_eq!(second["reasoning_effort"], "none");
        assert_eq!(second["model"], "gpt-5.6-luna");
        assert!(second.get("tools").is_some());
        reply(
            &mut stream,
            "200 OK",
            &json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "hi" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 2 }
            }),
        );
    });

    // `from_getter`'s closure, not real process env: parallel tests
    // cannot race each other over `AI_CHAT_ALLOW_INSECURE_HTTP`.
    let config = OpenAiConfig::from_getter(move |key| match key {
        "AI_CHAT_API_KEY" => Some("test-key".to_string()),
        "AI_CHAT_MODEL" => Some("gpt-5.6-luna".to_string()),
        "AI_CHAT_REASONING_EFFORT" => Some("medium".to_string()),
        "AI_CHAT_BASE_URL" => Some(format!("http://{addr}/v1")),
        "AI_CHAT_ALLOW_INSECURE_HTTP" => Some("1".to_string()),
        "AI_CHAT_TIMEOUT_SECS" => Some("5".to_string()),
        _ => None,
    });
    let client = ChatGptClient::try_from_config(&config).expect("client should initialize");

    let tools = vec![json!({ "type": "function", "function": { "name": "execute" } })];
    let messages = vec![json!({ "role": "user", "content": "hi" })];
    let options = ChatRequestOptions::new().with_tools(Some(tools));

    let result = client.send_chat(&messages, &options, None);

    server.join().expect("fixture server thread panicked");
    let data = result.expect("the corrected retry should succeed");
    assert_eq!(data["choices"][0]["message"]["content"], "hi");
    assert!(client.reasoning_none_forced());
}

#[test]
fn error_message_prefers_the_api_message() {
    let body =
        r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
    let message = error_message_from_body(body, StatusCode::UNAUTHORIZED);
    assert_eq!(message, "Incorrect API key provided");
}

#[test]
fn error_message_falls_back_to_status_and_raw_text() {
    assert_eq!(
        error_message_from_body("", StatusCode::BAD_GATEWAY),
        "Bad Gateway"
    );
    assert_eq!(
        error_message_from_body("upstream exploded", StatusCode::BAD_GATEWAY),
        "upstream exploded"
    );
}

/// A provider can report a mid-stream failure as its own SSE frame
/// (`data: {"error": {...}}`) rather than a non-2xx status. Without a
/// dedicated check this frame has no `choices` array, so
/// `DeltaAggregator::apply`'s early return would silently swallow it as
/// an empty heartbeat instead of surfacing the failure.
#[test]
fn apply_stream_payload_surfaces_an_in_band_error_frame() {
    let mut aggregator = DeltaAggregator::new();
    let mut delta_sent = false;
    let mut deltas = Vec::new();

    let result = ChatGptClient::apply_stream_payload(
        r#"{"error":{"message":"content policy violation"}}"#,
        &mut aggregator,
        &mut delta_sent,
        &mut |text| deltas.push(text.to_string()),
    );

    match result {
        Err(StreamError::BeforeFirstDelta(err)) => {
            assert!(err.to_string().contains("content policy violation"));
        }
        other => panic!("expected an in-band error to be surfaced, got {other:?}"),
    }
    assert!(deltas.is_empty());
}

/// The same error arriving after content has already streamed must not
/// be retried - some of the answer is already on the screen.
#[test]
fn apply_stream_payload_in_band_error_after_a_delta_is_not_retryable() {
    let mut aggregator = DeltaAggregator::new();
    let mut delta_sent = true;
    let mut deltas = Vec::new();

    let result = ChatGptClient::apply_stream_payload(
        r#"{"error":{"message":"backend overloaded"}}"#,
        &mut aggregator,
        &mut delta_sent,
        &mut |text| deltas.push(text.to_string()),
    );

    assert!(matches!(result, Err(StreamError::AfterFirstDelta(_))));
}

#[test]
fn apply_stream_payload_forwards_a_normal_delta() {
    let mut aggregator = DeltaAggregator::new();
    let mut delta_sent = false;
    let mut deltas = Vec::new();

    let result = ChatGptClient::apply_stream_payload(
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"}}]}"#,
        &mut aggregator,
        &mut delta_sent,
        &mut |text| deltas.push(text.to_string()),
    );

    assert!(result.is_ok());
    assert!(delta_sent);
    assert_eq!(deltas, vec!["hi".to_string()]);
}

#[test]
fn parse_retry_after_reads_seconds() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
    assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(2)));

    let empty = reqwest::header::HeaderMap::new();
    assert_eq!(parse_retry_after(&empty), None);
}
