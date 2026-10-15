//! Wire regressions use loopback endpoints injected only in this private test module.
use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

fn configured(provider: &str) -> ChatGptClient {
    let config = OpenAiConfig::from_getter(|key| match key {
        "AI_CHAT_PROVIDER" => Some(provider.into()),
        "AI_CHAT_OPENCODE_GO_MODEL" | "AI_CHAT_OPENCODE_ZEN_MODEL" => Some("glm-5.3".into()),
        "OPENCODE_API_KEY" => Some("fake-opencode-secret".into()),
        _ => None,
    });
    ChatGptClient::try_from_config(&config).unwrap()
}
fn read(stream: &mut TcpStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
    let size = headers
        .lines()
        .find_map(|v| v.strip_prefix("content-length:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    (headers, serde_json::from_slice(&body).unwrap())
}
fn header(headers: &str, name: &str) -> Option<String> {
    headers
        .lines()
        .find_map(|v| v.strip_prefix(&format!("{name}:")))
        .map(|v| v.trim().into())
}
fn reply(stream: &mut TcpStream, status: &str, extra: &str, body: &str) {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n{body}", body.len()).unwrap();
}
const OK: &str = r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":2}}"#;
fn stub_client(provider: &str, listener: &TcpListener) -> ChatGptClient {
    let mut client = configured(provider);
    client.chat_endpoint = format!(
        "http://{}/inference/go/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    // Same redirect policy as production; no inherited proxy for loopback tests.
    client.client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    client
}

#[test]
fn opencode_stream_recovery_keeps_session_auth_tools_and_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = stub_client("opencode_go", &listener);
    let server = std::thread::spawn(move || {
        let mut ids = Vec::new();
        // Rejected stream -> nonstream fallback, followed by an auxiliary request.
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let (headers, body) = read(&mut stream);
            assert!(headers.starts_with("post /inference/go/v1/chat/completions "));
            assert_eq!(
                header(&headers, "authorization").unwrap(),
                "bearer fake-opencode-secret"
            );
            assert!(
                header(&headers, "user-agent")
                    .unwrap()
                    .starts_with("doge-shell/")
            );
            ids.push(header(&headers, "x-opencode-session").unwrap());
            assert!(body.get("session_id").is_none());
            assert_eq!(body["model"], "glm-5.3");
            if index == 0 {
                assert_eq!(body["stream"], true);
                reply(
                    &mut stream,
                    "400 Bad Request",
                    "",
                    r#"{"error":{"message":"Unsupported parameter: stream"}}"#,
                );
            } else {
                if index == 1 {
                    assert!(body.get("tools").is_some());
                }
                reply(&mut stream, "200 OK", "", OK);
            }
        }
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
        assert!(!ids[0].contains("private"));
    });
    let options = ChatRequestOptions::new()
        .with_session_id(Some("private/project/conversation".into()))
        .with_stream(true)
        .with_tools(Some(vec![
            json!({"type":"function","function":{"name":"execute"}}),
        ]));
    let result = client
        .send_chat_streaming(&[], &options, None, &mut |_| {})
        .unwrap();
    assert_eq!(result["usage"]["prompt_tokens"], 7);
    client
        .send_chat(
            &[],
            &ChatRequestOptions::new().with_session_id(options.session_id.clone()),
            None,
        )
        .unwrap();
    server.join().unwrap();
}

#[test]
fn concurrent_clones_and_single_shot_calls_have_separate_session_ids() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = stub_client("opencode_zen", &listener);
    let server = std::thread::spawn(move || {
        let mut ids = std::collections::HashMap::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let (headers, body) = read(&mut stream);
            let name = body["messages"][0]["content"].as_str().unwrap().to_owned();
            ids.insert(name, header(&headers, "x-opencode-session").unwrap());
            reply(&mut stream, "200 OK", "", OK);
        }
        assert_ne!(ids["a"], ids["b"]);
        assert_ne!(ids["single1"], ids["single2"]);
        ids
    });
    std::thread::scope(|scope| {
        for id in ["a", "b"] {
            let cloned = client.clone();
            scope.spawn(move || {
                cloned
                    .send_chat(
                        &[json!({"role":"user","content":id})],
                        &ChatRequestOptions::new().with_session_id(Some(id.into())),
                        None,
                    )
                    .unwrap()
            });
        }
    });
    for id in ["single1", "single2"] {
        client
            .send_chat(
                &[json!({"role":"user","content":id})],
                &ChatRequestOptions::new(),
                None,
            )
            .unwrap();
    }
    let ids = server.join().unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(ids["a"], format!("{:x}", Sha256::digest(b"a")));
    assert_eq!(ids["b"], format!("{:x}", Sha256::digest(b"b")));
}

#[test]
fn dedicated_transport_does_not_follow_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let client = stub_client("opencode_go", &listener);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read(&mut stream);
        reply(
            &mut stream,
            "302 Found",
            &format!(
                "Location: http://{}/leak\r\n",
                destination.local_addr().unwrap()
            ),
            "{}",
        );
        destination
    });
    let error = client
        .send_chat(&[], &ChatRequestOptions::new(), None)
        .unwrap_err();
    assert_eq!(error.downcast_ref::<ApiError>().unwrap().status, Some(302));
    assert!(server.join().unwrap().accept().is_err());
}

#[test]
fn overrides_are_checked_before_network_and_generic_has_no_opencode_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let client = stub_client("opencode_go", &listener);
    for (stream, model) in [
        (false, "gpt-6-luna"),
        (true, "gpt-6-luna"),
        (false, ""),
        (true, "  "),
    ] {
        let options = ChatRequestOptions::new().with_model(Some(model.into()));
        let error = if stream {
            client.send_chat_streaming(&[], &options, None, &mut |_| {})
        } else {
            client.send_chat(&[], &options, None)
        }
        .unwrap_err();
        assert!(error.to_string().contains("chat_provider models"));
    }
    assert!(
        listener.accept().is_err(),
        "invalid models must not send a request"
    );
    let generic = ChatGptClient::new("generic-key".into()).unwrap();
    let request = generic
        .for_request(&ChatRequestOptions::new().with_session_id(Some("a".into())))
        .unwrap()
        .request_builder(&json!({}))
        .build()
        .unwrap();
    assert!(!request.headers().contains_key("x-opencode-session"));
    assert!(!request.headers().contains_key("user-agent"));
    let error = client.provider_error(
        ApiError {
            status: Some(401),
            retry_after: None,
            message: "fake-opencode-secret denied".into(),
        }
        .into(),
    );
    assert!(!error.to_string().contains("fake-opencode-secret"));
    assert!(error.to_string().contains("OpenCode API key"));
}

#[test]
fn server_errors_redact_before_truncation_and_retry_diagnostics() {
    let client = configured("opencode_go");
    let message = format!("{}{}", "x".repeat(390), client.api_key);
    let body = json!({"error":{"message":message}});
    for safe in [
        client.server_error_value(&body).unwrap(),
        client.server_error_body(&body.to_string(), StatusCode::TOO_MANY_REQUESTS),
        client.server_error_body(&message, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        assert!(!safe.contains("fake-openc"));
        assert!(safe.contains("[redacted]"));
    }
}
