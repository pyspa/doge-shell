//! The three command flows share a strict response boundary, offline.
use super::{MockAiService, expand_smart_pipe, fix_command, run_generative_command};

async fn candidate(response: &str, flow: usize) -> anyhow::Result<String> {
    let service = MockAiService::new(response);
    match flow {
        0 => run_generative_command(&service, "candidate").await,
        1 => fix_command(&service, "bad", 127, "missing").await,
        _ => expand_smart_pipe(&service, "candidate").await,
    }
}

#[tokio::test]
async fn rejects_invalid_schema_or_envelope_in_every_flow() {
    for response in [
        r#"{"command":"echo","args":[],"shell":"touch marker"}"#,
        r#"{"command":"echo","command":"rm","args":[]}"#,
        r#"{"command":"echo","args":[],"args":["second"]}"#,
        r#"{"command":"echo"}"#,
        r#"{"args":[]}"#,
        r#"{"command":null,"args":[]}"#,
        r#"{"command":42,"args":[]}"#,
        r#"{"command":"echo","args":"hello"}"#,
        r#"{"command":"echo","args":[null]}"#,
        r#"{"command":"echo","args":[1]}"#,
        r#"[{"command":"echo","args":[]}]"#,
        r#"["echo",[]]"#,
        "\u{a0}{\"command\":\"echo\",\"args\":[]}",
        "{\"command\":\"echo\",\"args\":[]}\u{a0}",
        "null",
        "",
        r#"{"command":"echo","args":[],}"#,
        r#"{"command":"echo","args":[]"#,
        r#"{"command":"echo","args":[]} {}"#,
        "Here is JSON: {\"command\":\"echo\",\"args\":[]}",
        "{\"command\":\"echo\",\"args\":[]}\nExplanation",
        "`{\"command\":\"echo\",\"args\":[]}`",
        "```json\n{\"command\":\"echo\",\"args\":[]}",
        "{\"command\":\"echo\",\"args\":[]}\n```",
        "```bash\n{\"command\":\"echo\",\"args\":[]}\n```",
        "```json\n{\"command\":\"echo\",\"args\":[]}\n```\nExplanation",
    ] {
        for flow in 0..3 {
            assert!(
                candidate(response, flow).await.is_err(),
                "flow {flow}: {response}"
            );
        }
    }
}

#[tokio::test]
async fn accepts_exact_json_and_complete_legacy_json_fences() {
    let json = r#"{"args":["日本語","","a b"],"command":"echo"}"#;
    for response in [
        json.to_owned(),
        format!(" \n{json}\r\n "),
        format!("```json\n{json}\n```"),
        format!("```\n{json}\n```"),
        format!("```json\r\n{json}\r\n```"),
    ] {
        for flow in 0..3 {
            assert_eq!(
                candidate(&response, flow).await.unwrap(),
                "\"echo\" \"日本語\" '' \"a b\""
            );
        }
    }
}

#[tokio::test]
async fn malformed_response_errors_do_not_echo_provider_text() {
    for response in [
        r#"{"command":"PRIVATE_COMMAND","args":["PRIVATE_ARG"],"PRIVATE_FIELD":"PRIVATE_VALUE"}"#,
        "PRIVATE_PROVIDER_TEXT",
    ] {
        for flow in 0..3 {
            let error = candidate(response, flow).await.unwrap_err().to_string();
            assert!(!error.contains("PRIVATE"), "{error}");
            assert!(error.contains("AI command JSON"), "{error}");
        }
    }
}
