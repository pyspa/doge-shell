//! Mock responses must become one literal argv, with no expansion or syntax.
use super::{MockAiService, expand_smart_pipe, fix_command, run_generative_command};
use crate::environment::Environment;
use crate::shell::{parse::parse_execution_plan, plan::WordPart};
use serde_json::json;

#[tokio::test]
async fn literal_argv_round_trip_for_every_command_flow() {
    let args = vec![
        "日本語",
        "a b",
        "",
        "a'b\"c",
        "*.rs",
        "a;b",
        "$(echo injected)",
        "line1\nline2",
        "C:\\path\\",
        "a\\'b",
        "$HOME",
        "~",
        "{a,b}",
        "#comment",
        "a|b",
        "`echo x`",
    ];
    let response = json!({"command": "echo", "args": args}).to_string();
    for flow in 0..3 {
        let service = MockAiService::new(&response);
        let rendered = match flow {
            0 => run_generative_command(&service, "literal values").await,
            1 => fix_command(&service, "bad", 127, "missing").await,
            _ => expand_smart_pipe(&service, "literal values").await,
        }
        .unwrap();
        let messages = service.last_messages.lock().unwrap();
        let prompt = messages[0]["content"].as_str().unwrap();
        assert!(prompt.contains("literal argv") && prompt.contains("JSON escaping only"));
        drop(messages);
        let environment = Environment::new();
        environment
            .write()
            .variable_state
            .alias
            .insert("echo".into(), "printf %s prefix".into());
        let plan = parse_execution_plan(&rendered, environment).unwrap();
        let jobs: Vec<_> = plan.iter_jobs().collect();
        assert_eq!(jobs.len(), 1, "{rendered}");
        assert_eq!(jobs[0].stages.len(), 1);
        let stage = &jobs[0].stages[0];
        assert!(stage.redirects.is_empty() && stage.env_overrides.is_empty());
        let actual: Vec<String> = stage
            .argv
            .iter()
            .map(|word| {
                assert!(
                    !word.parts.is_empty(),
                    "empty word must preserve an argv field"
                );
                word.parts
                    .iter()
                    .map(|part| {
                        let WordPart::Literal(literal) = part else {
                            panic!("expansion in {rendered}")
                        };
                        assert!(
                            !literal.pattern_active
                                && !literal.brace_active
                                && !literal.tilde_candidate,
                            "active pattern in {rendered}"
                        );
                        literal.text.as_str()
                    })
                    .collect()
            })
            .collect();
        let expected: Vec<_> = std::iter::once("echo")
            .chain(args.iter().copied())
            .collect();
        assert_eq!(actual, expected, "{rendered}");
    }
}

#[tokio::test]
async fn command_name_is_one_literal_word() {
    for command in [
        "日本語 command",
        "if",
        "X=value",
        "echo; touch marker",
        "$(echo injected)",
    ] {
        let response = json!({"command": command, "args": []}).to_string();
        let service = MockAiService::new(&response);
        let source = run_generative_command(&service, "name").await.unwrap();
        let plan = parse_execution_plan(&source, Environment::new()).unwrap();
        let jobs: Vec<_> = plan.iter_jobs().collect();
        assert_eq!(jobs.len(), 1);
        let stage = &jobs[0].stages[0];
        assert!(stage.env_overrides.is_empty());
        assert_eq!(stage.argv.len(), 1);
        let text: String = stage.argv[0]
            .parts
            .iter()
            .map(|p| {
                let WordPart::Literal(literal) = p else {
                    panic!("active name: {source}")
                };
                literal.text.as_str()
            })
            .collect();
        assert_eq!(text, command);
    }
}

#[tokio::test]
async fn invalid_literal_values_are_rejected_in_every_flow() {
    for (command, args) in [("", vec![]), ("ec\0ho", vec![]), ("echo", vec!["a\0b"])] {
        let response = json!({"command": command, "args": args}).to_string();
        let service = MockAiService::new(&response);
        assert!(run_generative_command(&service, "values").await.is_err());
        assert!(fix_command(&service, "bad", 1, "").await.is_err());
        assert!(expand_smart_pipe(&service, "values").await.is_err());
    }
}

#[tokio::test]
async fn confirmation_required_candidate_is_still_returned() {
    use crate::ai_features::service::AiService;
    use crate::safety::{SafetyGuard, SafetyLevel, SafetyResult};
    use std::sync::Arc;
    struct Guarded;
    #[async_trait::async_trait]
    impl AiService for Guarded {
        async fn send_request(
            &self,
            _: Vec<serde_json::Value>,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(json!({"command":"rm", "args":["-rf", "/"]}).to_string())
        }
        fn get_safety_guard(&self) -> Option<Arc<SafetyGuard>> {
            Some(Arc::new(SafetyGuard::new()))
        }
        fn get_safety_level(&self) -> Option<SafetyLevel> {
            Some(SafetyLevel::Strict)
        }
        fn get_allowlist(&self) -> Option<Vec<String>> {
            Some(vec![])
        }
    }
    assert!(matches!(
        SafetyGuard::new().check_command(
            &SafetyLevel::Strict,
            "rm",
            &["-rf".into(), "/".into()],
            &[]
        ),
        SafetyResult::Confirm(_)
    ));
    assert_eq!(
        run_generative_command(&Guarded, "candidate only")
            .await
            .unwrap(),
        "\"rm\" -rf /"
    );
}
