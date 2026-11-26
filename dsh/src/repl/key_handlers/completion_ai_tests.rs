#[test]
fn smart_pipe_marker_is_only_top_level_and_ai_query_remains_opaque() {
    use crate::ai_task_input::{TabAction, tab_action};
    for input in [
        "echo # |? ignored",
        "echo '|? ignored'",
        "echo \\|?ignored",
        "echo $(printf '|?')",
        "echo |: select x |? ignored",
        "echo |> x |? ignored",
    ] {
        assert_eq!(
            tab_action(input, input.chars().count()),
            TabAction::Shell,
            "{input:?}"
        );
    }
    assert_eq!(
        tab_action("echo |? summarize # text |? remains query", 7),
        TabAction::ExpandPipe {
            marker: 5,
            query: "summarize # text |? remains query"
        }
    );
}

#[tokio::test]
async fn cancelled_ai_completion_retains_query_and_completed_retry_replaces_it() {
    use crate::environment::Environment;
    use crate::repl::Repl;
    use crate::shell::Shell;
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    for (query, pipe_marker, expected) in [
        ("?? 日本語 query", None, "\"echo\" retry"),
        (
            "cat payload |? 日本語 query",
            Some("cat payload ".len()),
            "cat payload | \"echo\" retry",
        ),
    ] {
        repl.input.reset(query.to_string());
        let cursor = repl.input.cursor();
        super::apply_ai_completion(&mut repl, None, pipe_marker);
        assert_eq!(repl.input.as_str(), query);
        assert_eq!(repl.input.cursor(), cursor);
        super::apply_ai_completion(&mut repl, Some("\"echo\" retry".into()), pipe_marker);
        assert_eq!(repl.input.as_str(), expected);
    }
}
#[tokio::test]
async fn delayed_ai_tab_routes_use_mock_service_and_allow_fresh_requests() {
    use crate::ai_features::AiService;
    use crate::environment::Environment;
    use crate::repl::Repl;
    use crate::shell::Shell;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Delayed(AtomicUsize);
    #[async_trait::async_trait]
    impl AiService for Delayed {
        async fn send_request(
            &self,
            _: Vec<serde_json::Value>,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            let id = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            Ok(serde_json::json!({"command":"echo", "args":[id.to_string()]}).to_string())
        }
    }
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    // Configure availability after construction; every Tab request uses
    // the mock below, with no live API or background request involved.
    {
        let mut env = repl.shell.environment.write();
        env.set_shell_var("AI_CHAT_PROVIDER".into(), "api_key".into());
        env.set_shell_var("AI_CHAT_ALLOW_INSECURE_HTTP".into(), "true".into());
        env.set_shell_var("AI_CHAT_BASE_URL".into(), "http://127.0.0.1:9/v1".into());
        env.set_shell_var("AI_CHAT_API_KEY".into(), "mock-only".into());
    }
    let service = Arc::new(Delayed(AtomicUsize::new(0)));
    repl.services.ai = Some(service.clone());
    let input = "echo # 編集中\ncat |? summarize";
    repl.input.reset(input.to_owned());
    repl.input.move_to(8);
    super::handle_trigger_completion(&mut repl).await.unwrap();
    assert_eq!(service.0.load(Ordering::SeqCst), 0);
    assert_eq!(repl.input.as_str(), input);
    let tab = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    );
    for input in [
        "! explain |? literally",
        "!& investigate |? literally",
        "echo 界 |! explain |? literally",
        "echo |! explain | git che",
        "| cat |! explain |? literally",
    ] {
        repl.input.reset(input.into());
        repl.ai_ui.last_analyzed_input = "from previous redraw".into();
        repl.handle_key_event(&tab).await.unwrap();
        assert_eq!(
            repl.ai_ui.last_analyzed_input, "from previous redraw",
            "{input}"
        );
        assert_eq!(service.0.load(Ordering::SeqCst), 0, "{input}");
        assert_eq!(repl.input.as_str(), input);
    }
    for (prefix, line) in [
        ("echo 日 |! explain \\\n", "why |? now"),
        ("echo 'open\n", "|? literal_in_quote"),
        ("echo 'open\n", "| git che"),
        ("echo a |\n", "| git che"),
    ] {
        repl.state.multiline_buffer = prefix.into();
        repl.input.reset(line.into());
        repl.handle_key_event(&tab).await.unwrap();
        assert_eq!(service.0.load(Ordering::SeqCst), 0, "{prefix}{line}");
        assert_eq!(repl.input.as_str(), line);
        assert_eq!(repl.state.multiline_buffer, prefix);
    }
    repl.state.multiline_buffer.clear();
    for (query, expected) in [
        ("?? 日本語 query", "echo 1"),
        ("cat payload |? 日本語 query", "cat payload | echo 2"),
        ("?? 日本語 |? literal query", "echo 3"),
    ] {
        repl.input.reset(query.to_string());
        super::handle_trigger_completion(&mut repl).await.unwrap();
        // Executable quoting belongs to the separate literal-argv change.
        assert_eq!(repl.input.as_str().replace("\"echo\"", "echo"), expected);
    }
    for (prefix, line, expected_prefix, expected_line) in [
        ("echo 日 |\n", "cat |? query", "echo 日 |\n", "cat | echo 4"),
        ("echo 日 |? what is '\n", "this", "", "echo 日 | echo 5"),
        ("?? what is '\n", "this", "", "echo 6"),
    ] {
        repl.state.multiline_buffer = prefix.into();
        // A real continuation line starts with an empty undo stack.
        repl.input.clear();
        repl.input.insert_str(line);
        crate::repl::render::print_prompt(&mut repl, &mut Vec::new());
        assert_eq!(repl.terminal_ui.prompt_mark_cache, "..> ");
        repl.handle_key_event(&tab).await.unwrap();
        assert_eq!(repl.state.multiline_buffer, expected_prefix);
        assert_eq!(
            repl.input.as_str().replace("\"echo\"", "echo"),
            expected_line
        );
        if expected_prefix.is_empty() {
            assert_eq!(
                repl.terminal_ui.prompt_mark_cache,
                repl.terminal_ui.prompt.read().mark
            );
        }
        assert!(repl.input.undo());
        assert_eq!(
            format!("{}{}", repl.state.multiline_buffer, repl.input.as_str()),
            format!("{prefix}{line}")
        );
        assert!(repl.input.undo());
        assert_eq!(
            format!("{}{}", repl.state.multiline_buffer, repl.input.as_str()),
            prefix
        );
    }
    assert_eq!(service.0.load(Ordering::SeqCst), 6);
}
