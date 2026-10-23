use crate::completion;
use crate::completion::MAX_RESULT;
use crate::completion::integrated::{
    CompletionDisposition, CompletionReplacementRange, CompletionResult,
};
use crate::input::Input;
use crate::repl::Repl;
use crate::repl::state::{ReplControlFlow, SuggestionAcceptMode};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use tracing::{debug, warn};

pub(crate) fn handle_accept_suggestion_word(repl: &mut Repl<'_>) -> bool {
    if repl.accept_suggestion(SuggestionAcceptMode::Word) {
        return true; // reset_completion = true
    }
    false
}

pub(crate) fn handle_accept_suggestion_full(repl: &mut Repl<'_>) -> bool {
    if repl.input.is_empty()
        && repl
            .ai_ui
            .auto_fix_suggestion
            .as_ref()
            .is_some_and(|fix| fix.has_fix())
    {
        if let Some(fix) = repl.ai_ui.auto_fix_suggestion.take() {
            repl.input.reset(fix.replacement);
            repl.refresh_inline_suggestion(); // clear potential other suggestions
            return true; // reset_completion = true
        }
    } else if repl.accept_active_suggestion() {
        repl.completion_ui.completion.clear();
        return true; // reset_completion = true
    }
    false
}

pub(crate) fn handle_rotate_suggestion_forward(repl: &mut Repl<'_>) -> bool {
    if repl.ai_ui.suggestion_manager.rotate(1) {
        return false; // reset_completion = false
    }
    false
}

pub(crate) fn handle_rotate_suggestion_backward(repl: &mut Repl<'_>) -> bool {
    if repl.ai_ui.suggestion_manager.rotate(-1) {
        return false; // reset_completion = false
    }
    false
}

pub(crate) fn handle_accept_completion(repl: &mut Repl<'_>) {
    if let Some(comp) = &repl.input.completion.take() {
        repl.input.reset(comp.to_string());
    }
    repl.completion_ui.completion.clear();
}

pub(crate) fn handle_cancel_completion(repl: &mut Repl<'_>) {
    if repl.input.completion.is_some() || repl.ai_ui.suggestion_manager.active.is_some() {
        repl.completion_ui.completion.clear();
        repl.ai_ui.suggestion_manager.clear();
        let mut renderer = TerminalRenderer::new();
        repl.print_input(&mut renderer, true, true);
        renderer.flush().ok();
    }
}

// A cancelled wait is a handled Tab action: retain the query and do not open
// ordinary completion. Only a completed request can replace the input.
fn apply_ai_completion(repl: &mut Repl<'_>, expanded: Option<String>, smart_pipe: bool) {
    let Some(expanded) = expanded else { return };
    let replacement = if smart_pipe {
        let Some(idx) = crate::completion::command_segment::smart_pipe_marker(repl.input.as_str())
        else {
            return;
        };
        format!("{}| {expanded}", &repl.input.as_str()[..idx])
    } else {
        expanded
    };
    repl.input.reset(replacement);
    repl.completion_ui.completion.clear();
}

pub(crate) async fn handle_trigger_completion(repl: &mut Repl<'_>) -> Result<ReplControlFlow> {
    // Check for Smart Pipe Expansion (|? query)
    if let Some(smart_pipe_query) = repl.detect_smart_pipe() {
        match repl.expand_smart_pipe(smart_pipe_query).await {
            Ok(expanded) => {
                apply_ai_completion(repl, expanded, true);
                return Ok(ReplControlFlow::Continue);
            }
            Err(e) => {
                warn!("Smart pipe expansion failed: {}", e);
                notify_ai_failure(repl, "smart pipe", &e);
            }
        }
    }

    // Check for Generative Command Expansion (?? query)
    if let Some(generative_query) = repl.detect_generative_command() {
        match repl.run_generative_command(&generative_query).await {
            Ok(expanded) => {
                apply_ai_completion(repl, expanded, false);
                return Ok(ReplControlFlow::Continue);
            }
            Err(e) => {
                warn!("Generative command expansion failed: {}", e);
                notify_ai_failure(repl, "generated command", &e);
            }
        }
    }

    // Classify before hooks, providers, legacy fallback and suggestions. An
    // empty allowed word has a zero-width range; suppressed syntax has none.
    let Some(lexical_range) =
        completion::integrated::token_range_at_cursor(repl.input.as_str(), repl.input.cursor())
    else {
        return Ok(ReplControlFlow::Continue);
    };

    // The inline grid measures the terminal and draws at its bottom edge, so
    // the reserved status row has to come back first.
    let _status_pause =
        crate::repl::status_line::StatusLinePause::new(repl.terminal_ui.status_line.clone());

    // Extract the current word at cursor position for completion query
    let completion_query_owned = completion_query_for_input(&repl.input);
    let completion_query = completion_query_owned.as_deref();

    // Get the current prompt text and input text for completion display context
    let prompt_text = repl.terminal_ui.prompt.read().mark.clone();
    let input_text = repl.input.to_string();

    debug!(
        "TAB completion starting with prompt: '{}', input: '{}', query: '{:?}'",
        prompt_text, input_text, completion_query
    );

    // Execute completion hooks
    let _ = repl
        .shell
        .exec_completion_hooks(&input_text, repl.input.cursor());

    // Use the new integrated completion engine with current directory context
    let current_dir = repl.terminal_ui.prompt.read().current_path().to_path_buf();
    let cursor_pos = repl.input.cursor();

    debug!(
        "Using IntegratedCompletionEngine for input: '{}' at position {}",
        input_text, cursor_pos
    );

    repl.sync_completion_jobs();

    // Cold dynamic providers only schedule work on their dedicated workers.
    // The initial TAB therefore returns static/cached candidates immediately;
    // the completion notifier asks the REPL to refresh when dynamic data lands.
    let CompletionResult {
        disposition,
        candidates: engine_candidates,
        framework: completion_framework,
        replacement_range,
    } = repl
        .completion_ui
        .integrated_completion
        .complete(
            input_text.as_str(),
            cursor_pos,
            current_dir.as_path(),
            MAX_RESULT,
            repl.shell.cmd_history.as_ref(),
        )
        .await;

    if disposition == CompletionDisposition::Suppressed {
        return Ok(ReplControlFlow::Continue);
    }
    let replacement_range = replacement_range.or(Some(lexical_range));
    let expected_cursor = cursor_pos;

    debug!(
        "IntegratedCompletionEngine returned {} candidates (framework: {:?})",
        engine_candidates.len(),
        completion_framework
    );

    // Attempt to get completion result
    // First try with integrated engine
    if !engine_candidates.is_empty() {
        let raw_token = replacement_range.map(|range| range_text(&input_text, range));
        let selection_query_owned = replacement_range.map(|range| {
            let prefix = range_text(
                &input_text,
                CompletionReplacementRange {
                    start: range.start,
                    end: expected_cursor.min(range.end),
                },
            );
            completion::shell_path::normalize_path_token(&prefix)
        });
        let selection_query = selection_query_owned.as_deref().or(completion_query);

        // If integrated engine returned candidates, show them with skim selector
        let completion_candidates: Vec<completion::Candidate> = repl
            .completion_ui
            .integrated_completion
            .to_candidates(engine_candidates);
        let completion_candidates = completion::shell_path::format_candidates_for_token(
            completion_candidates,
            raw_token.as_deref(),
        );

        let res = completion::select_completion_items_with_framework(
            completion_candidates,
            selection_query,
            &prompt_text,
            &input_text,
            crate::completion::CompletionConfig::default(),
            completion_framework,
        );

        match res {
            completion::CompletionSelection::Selected(val) => {
                debug!("Completion selected: '{}'", val);
                if let Some(range) = replacement_range {
                    crate::repl::state::apply_completion_range(
                        &mut repl.input,
                        &input_text,
                        expected_cursor,
                        range.start,
                        range.end,
                        &val,
                    );
                }
                repl.completion_ui.start_completion = true;
                return Ok(ReplControlFlow::Continue);
            }
            completion::CompletionSelection::Interactive(items, query) => {
                // Return control flow to run interactive completion (Skim)
                let query = query.unwrap_or_default();
                let candidates = items;
                return Ok(ReplControlFlow::RunInteractive(Box::new(move || {
                    use crate::completion::framework::SkimCompletionFramework;
                    let result = SkimCompletionFramework::run_with_skim(candidates, Some(query));
                    Ok(result.map(|text| {
                        completion_action(
                            text,
                            replacement_range,
                            input_text.clone(),
                            expected_cursor,
                        )
                    }))
                })));
            }
            completion::CompletionSelection::Input(text) => {
                apply_inline_input(&mut repl.input, &input_text, expected_cursor, &text);
                repl.completion_ui.start_completion = true;
                return Ok(ReplControlFlow::Continue);
            }
            completion::CompletionSelection::None => {
                // Fallthrough to legacy completion below
            }
        }
    }

    // If no candidates from integrated engine, fall back to legacy completion system
    debug!("No candidates from IntegratedCompletionEngine, falling back to legacy completion");
    let legacy_query = replacement_range.map(|range| {
        range_text(
            &input_text,
            CompletionReplacementRange {
                start: range.start,
                end: expected_cursor.min(range.end),
            },
        )
    });
    let completion_query = legacy_query.as_deref().or(completion_query);
    let raw_token = replacement_range.map(|range| range_text(&input_text, range));
    let completion_result = completion::input_completion(
        &repl.input,
        repl,
        completion_query,
        raw_token.as_deref(),
        &prompt_text,
        &input_text,
    )
    .await;

    // Process the completion result
    let mut completion_handled = false;
    match completion_result {
        completion::CompletionSelection::Selected(val) => {
            debug!("Completion selected: '{}'", val);
            if let Some(range) = replacement_range {
                crate::repl::state::apply_completion_range(
                    &mut repl.input,
                    &input_text,
                    expected_cursor,
                    range.start,
                    range.end,
                    &val,
                );
            }
            completion_handled = true;
        }
        completion::CompletionSelection::Interactive(items, query) => {
            let query = query.unwrap_or_default();
            return Ok(ReplControlFlow::RunInteractive(Box::new(move || {
                use crate::completion::framework::SkimCompletionFramework;
                let result = SkimCompletionFramework::run_with_skim(items, Some(query));
                Ok(result.map(|text| {
                    completion_action(text, replacement_range, input_text.clone(), expected_cursor)
                }))
            })));
        }
        completion::CompletionSelection::Input(text) => {
            apply_inline_input(&mut repl.input, &input_text, expected_cursor, &text);
            completion_handled = true;
        }
        completion::CompletionSelection::None => {
            // No completion found
        }
    }

    if !completion_handled {
        // Fallback: If we have an active suggestion, accept the next word of it.
        if repl.accept_suggestion(SuggestionAcceptMode::Word) {
            debug!("No standard completion, accepted suggestion word fallback");
            // completion_handled = true;
        } else {
            debug!("No completion selected and no suggestion to accept");
        }
    }

    // Force a redraw after completion to update the display
    repl.completion_ui.start_completion = true;
    Ok(ReplControlFlow::Continue)
}

fn apply_inline_input(input: &mut Input, expected_input: &str, expected_cursor: usize, text: &str) {
    if input.as_str() == expected_input && input.cursor() == expected_cursor {
        input.insert_str(text);
    }
}

fn completion_action(
    text: String,
    replacement_range: Option<CompletionReplacementRange>,
    expected_input: String,
    expected_cursor: usize,
) -> crate::repl::state::InteractiveAction {
    let range = replacement_range.expect("allowed completion always has a source range");
    crate::repl::state::InteractiveAction::ReplaceRange {
        expected_input,
        expected_cursor,
        start: range.start,
        end: range.end,
        text,
    }
}

fn completion_query_for_input(input: &Input) -> Option<String> {
    let range = completion::integrated::token_range_at_cursor(input.as_str(), input.cursor())?;
    Some(range_text(
        input.as_str(),
        CompletionReplacementRange {
            start: range.start,
            end: input.cursor().min(range.end),
        },
    ))
}

fn range_text(input: &str, range: CompletionReplacementRange) -> String {
    input
        .chars()
        .skip(range.start)
        .take(range.end.saturating_sub(range.start))
        .collect()
}

/// Surface an AI expansion failure to the user.
///
/// These paths used to only write to the trace log, so a request that the
/// safety policy rejected looked exactly like a key that did nothing.
fn notify_ai_failure(repl: &mut Repl<'_>, what: &str, err: &anyhow::Error) {
    let mut renderer = TerminalRenderer::new();
    let line = format!("dsh: {what}: {err}").replace(['\n', '\r'], " ");
    crate::repl::render::print_above_prompt(repl, &mut renderer, &[line]);
    renderer.flush().ok();
}

#[cfg(test)]
mod tests {
    use super::{completion_action, completion_query_for_input, range_text};
    use crate::completion::integrated::CompletionReplacementRange;
    use crate::input::{Input, InputConfig};
    use crate::repl::state::InteractiveAction;

    fn input_state(input: &str) -> Input {
        let mut state = Input::new(InputConfig::default());
        state.reset(input.to_string());
        state
    }

    #[tokio::test]
    async fn tab_preserves_suppressed_regions_and_replaces_middle_legacy_word() {
        use crate::{environment::Environment, repl::Repl, shell::Shell};
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture
            .path()
            .join("界e\u{301}-audit-unique-completion.txt");
        std::fs::write(&path, "fixture").unwrap();
        let prefix = path.to_string_lossy().trim_end_matches(".txt").to_string();
        for before in [
            format!("echo # {prefix}"),
            format!("echo |: select {prefix}"),
            format!("echo |> {prefix}"),
            format!("echo |:\n select {prefix}"),
        ] {
            let env = Environment::new_synthetic();
            env.write().variable_state.paths.clear();
            let mut shell = Shell::new(env);
            let mut repl = Repl::new(&mut shell);
            repl.input.reset(before.clone());
            super::handle_trigger_completion(&mut repl).await.unwrap();
            assert_eq!(repl.input.as_str(), before);
        }
        let env = Environment::new_synthetic();
        env.write().variable_state.paths.clear();
        let mut shell = Shell::new(env);
        let mut repl = Repl::new(&mut shell);
        let before = format!("git switch {prefix}.txt tail");
        repl.input.reset(before.clone());
        repl.input
            .move_to(format!("git switch {prefix}").chars().count());
        super::handle_trigger_completion(&mut repl).await.unwrap();
        assert_eq!(repl.input.as_str(), before);
    }

    #[test]
    fn inline_typing_inserts_source_at_middle_cursor_without_reencoding() {
        for (marked, expected) in [
            (r"echo dir\ with\ space/fo§", r"echo dir\ with\ space/fox"),
            ("probe ol§d tail", "probe olxd tail"),
            ("echo 界e§\u{301} tail", "echo 界ex\u{301} tail"),
            ("echo \"dir with space/fo§", "echo \"dir with space/fox"),
            ("echo 'dir with space/fo§", "echo 'dir with space/fox"),
            ("echo | fo§", "echo | fox"),
            ("echo | §", "echo | x"),
        ] {
            let cursor = marked[..marked.find('§').unwrap()].chars().count();
            let before = &marked.replace('§', "");
            let mut input = input_state(before);
            input.move_to(cursor);
            super::apply_inline_input(&mut input, before, cursor, "x");
            assert_eq!(input.as_str(), expected);
        }
    }

    #[test]
    fn inline_typing_rejects_stale_input_and_cursor() {
        for (new_input, cursor) in [("echo newer", 7), ("echo old", 8)] {
            let mut input = input_state(new_input);
            input.move_to(cursor);
            super::apply_inline_input(&mut input, "echo old", 7, "x");
            assert_eq!(input.as_str(), new_input);
            assert_eq!(input.cursor(), cursor);
        }
    }

    #[test]
    fn delayed_and_inline_completion_only_apply_to_original_editor_snapshot() {
        use crate::completion::{
            display::Candidate, framework::SkimCompletionFramework,
            shell_path::format_candidates_for_token,
        };
        use skim::SkimItem;
        let before = "echo 界 old tail";
        let candidates =
            format_candidates_for_token(vec![Candidate::Basic("hello world".into())], None);
        // Inline uses the displayed candidate's output; Skim's deterministic
        // single-result path shares its production output contract.
        for text in [
            candidates[0].output().to_string(),
            SkimCompletionFramework::run_with_skim(candidates.clone(), None).unwrap(),
        ] {
            let action = completion_action(
                text,
                Some(CompletionReplacementRange { start: 7, end: 10 }),
                before.into(),
                8,
            );
            let InteractiveAction::ReplaceRange {
                expected_input,
                expected_cursor,
                start,
                end,
                text,
            } = action
            else {
                panic!("range action required")
            };
            for (new_input, new_cursor, applies) in [
                (before, 8, true),
                (before, 9, false),
                ("echo newer text", 8, false),
            ] {
                let mut input = input_state(new_input);
                input.move_to(new_cursor);
                let applied = crate::repl::state::apply_completion_range(
                    &mut input,
                    &expected_input,
                    expected_cursor,
                    start,
                    end,
                    &text,
                );
                assert_eq!(applied, applies);
                assert_eq!(
                    input.as_str(),
                    if applies {
                        "echo 界 hello\\ world tail"
                    } else {
                        new_input
                    }
                );
            }
        }
    }

    #[test]
    fn smart_pipe_marker_is_only_top_level_and_ai_query_remains_opaque() {
        use crate::completion::command_segment::smart_pipe_marker;
        for input in [
            "echo # |? ignored",
            "echo '|? ignored'",
            "echo \\|?ignored",
            "echo $(printf '|?')",
            "echo |: select x |? ignored",
            "echo |> x |? ignored",
        ] {
            assert_eq!(smart_pipe_marker(input), None, "{input:?}");
        }
        assert_eq!(
            smart_pipe_marker("echo |? summarize # text |? remains query"),
            Some(5)
        );
    }

    #[test]
    fn integrated_replacement_range_creates_replace_range_action() {
        let action = completion_action(
            "dev-cluster".to_string(),
            Some(CompletionReplacementRange { start: 18, end: 20 }),
            "original".to_string(),
            12,
        );

        assert!(matches!(
            action,
            InteractiveAction::ReplaceRange {
                start: 18,
                end: 20,
                text, ..
            } if text == "dev-cluster"
        ));
    }

    #[test]
    fn range_text_uses_replacement_range() {
        assert_eq!(
            range_text(
                "kubectl --context=de",
                CompletionReplacementRange { start: 18, end: 20 }
            ),
            "de"
        );
        assert_eq!(
            range_text(
                "kubectl --context=",
                CompletionReplacementRange { start: 18, end: 18 }
            ),
            ""
        );
        assert_eq!(
            range_text(
                r#"cat "dir with space/foo"#,
                CompletionReplacementRange { start: 4, end: 23 }
            ),
            r#""dir with space/foo"#
        );
    }

    #[test]
    fn completion_query_fallback_preserves_unclosed_quoted_path_token() {
        let input = input_state(r#"cat "dir with space/fo"#);

        assert_eq!(
            completion_query_for_input(&input).as_deref(),
            Some(r#""dir with space/fo"#)
        );
    }

    #[test]
    fn completion_query_keeps_parser_result_when_available() {
        let input = input_state(r#"cat dir\ with\ space/fo"#);

        assert_eq!(
            completion_query_for_input(&input).as_deref(),
            Some(r#"dir\ with\ space/fo"#)
        );
    }
    #[tokio::test]
    async fn cancelled_ai_completion_retains_query_and_completed_retry_replaces_it() {
        use crate::environment::Environment;
        use crate::repl::Repl;
        use crate::shell::Shell;
        let mut shell = Shell::new(Environment::new());
        let mut repl = Repl::new(&mut shell);
        for (query, smart_pipe, expected) in [
            ("?? 日本語 query", false, "\"echo\" retry"),
            (
                "cat payload |? 日本語 query",
                true,
                "cat payload | \"echo\" retry",
            ),
        ] {
            repl.input.reset(query.to_string());
            let cursor = repl.input.cursor();
            super::apply_ai_completion(&mut repl, None, smart_pipe);
            assert_eq!(repl.input.as_str(), query);
            assert_eq!(repl.input.cursor(), cursor);
            super::apply_ai_completion(&mut repl, Some("\"echo\" retry".into()), smart_pipe);
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
        for (query, expected) in [
            ("?? 日本語 query", "echo 1"),
            ("cat payload |? 日本語 query", "cat payload | echo 2"),
            ("?? 日本語 query", "echo 3"),
        ] {
            repl.input.reset(query.to_string());
            super::handle_trigger_completion(&mut repl).await.unwrap();
            // Executable quoting belongs to the separate literal-argv change.
            assert_eq!(repl.input.as_str().replace("\"echo\"", "echo"), expected);
        }
        assert_eq!(service.0.load(Ordering::SeqCst), 3);
    }
}
