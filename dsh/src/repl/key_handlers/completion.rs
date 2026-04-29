use crate::completion;
use crate::completion::MAX_RESULT;
use crate::completion::integrated::{CompletionReplacementRange, CompletionResult};
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
    if repl.input.is_empty() && repl.auto_fix_suggestion.is_some() {
        if let Some(fix) = repl.auto_fix_suggestion.take() {
            repl.input.reset(fix);
            repl.refresh_inline_suggestion(); // clear potential other suggestions
            return true; // reset_completion = true
        }
    } else if repl.accept_active_suggestion() {
        repl.completion.clear();
        return true; // reset_completion = true
    }
    false
}

pub(crate) fn handle_rotate_suggestion_forward(repl: &mut Repl<'_>) -> bool {
    if repl.suggestion_manager.rotate(1) {
        return false; // reset_completion = false
    }
    false
}

pub(crate) fn handle_rotate_suggestion_backward(repl: &mut Repl<'_>) -> bool {
    if repl.suggestion_manager.rotate(-1) {
        return false; // reset_completion = false
    }
    false
}

pub(crate) fn handle_accept_completion(repl: &mut Repl<'_>) {
    if let Some(comp) = &repl.input.completion.take() {
        repl.input.reset(comp.to_string());
    }
    repl.completion.clear();
}

pub(crate) fn handle_cancel_completion(repl: &mut Repl<'_>) {
    if repl.input.completion.is_some() || repl.suggestion_manager.active.is_some() {
        repl.completion.clear();
        repl.suggestion_manager.clear();
        let mut renderer = TerminalRenderer::new();
        repl.print_input(&mut renderer, true, true);
        renderer.flush().ok();
    }
}

pub(crate) async fn handle_trigger_completion(repl: &mut Repl<'_>) -> Result<ReplControlFlow> {
    // Check for Smart Pipe Expansion (|? query)
    if let Some(smart_pipe_query) = repl.detect_smart_pipe() {
        match repl.expand_smart_pipe(smart_pipe_query).await {
            Ok(expanded) => {
                let input_str = repl.input.as_str();
                if let Some(idx) = input_str.rfind("|?") {
                    let prefix = &input_str[..idx];
                    let mut new_input = prefix.to_string();
                    new_input.push_str("| ");
                    new_input.push_str(&expanded);
                    repl.input.reset(new_input);
                    repl.completion.clear();
                    return Ok(ReplControlFlow::Continue);
                }
            }
            Err(e) => {
                warn!("Smart pipe expansion failed: {}", e);
            }
        }
    }

    // Check for Generative Command Expansion (?? query)
    if let Some(generative_query) = repl.detect_generative_command() {
        match repl.run_generative_command(&generative_query).await {
            Ok(expanded) => {
                repl.input.reset(expanded);
                repl.completion.clear();
                return Ok(ReplControlFlow::Continue);
            }
            Err(e) => {
                warn!("Generative command expansion failed: {}", e);
            }
        }
    }

    // Extract the current word at cursor position for completion query
    let completion_query_owned = completion_query_for_input(&repl.input);
    let completion_query = completion_query_owned.as_deref();
    let removal_len = completion_query_owned
        .as_ref()
        .map(|query| query.chars().count());

    // Get the current prompt text and input text for completion display context
    let prompt_text = repl.prompt.read().mark.clone();
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
    let current_dir = repl.prompt.read().current_path().to_path_buf();
    let cursor_pos = repl.input.cursor();

    debug!(
        "Using IntegratedCompletionEngine for input: '{}' at position {}",
        input_text, cursor_pos
    );

    // Get completion candidates from the integrated engine
    let CompletionResult {
        candidates: engine_candidates,
        framework: completion_framework,
        replacement_range,
    } = repl
        .integrated_completion
        .complete(
            &input_text,
            cursor_pos,
            &current_dir,
            MAX_RESULT, // maximum number of candidates to return
            repl.shell.cmd_history.as_ref(),
        )
        .await;

    debug!(
        "IntegratedCompletionEngine returned {} candidates (framework: {:?})",
        engine_candidates.len(),
        completion_framework
    );

    // Attempt to get completion result
    // First try with integrated engine
    if !engine_candidates.is_empty() {
        let replacement_query_owned = replacement_range.map(|range| range_text(&input_text, range));
        let selection_query = replacement_query_owned.as_deref().or(completion_query);

        // If integrated engine returned candidates, show them with skim selector
        let completion_candidates: Vec<completion::Candidate> =
            repl.integrated_completion.to_candidates(engine_candidates);
        let completion_candidates = completion::shell_path::format_candidates_for_token(
            completion_candidates,
            selection_query,
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
                // For history candidates (indicated by clock emoji), replace entire input
                let is_history_candidate = val.starts_with("🕒 ");
                if is_history_candidate {
                    let command = val[3..].trim();
                    repl.input.reset(command.to_string());
                } else {
                    if let Some(range) = replacement_range {
                        repl.input
                            .replace_range_chars(range.start, range.end, val.as_str());
                    } else {
                        if let Some(len) = removal_len {
                            repl.input.backspacen(len);
                        }
                        repl.input.insert_str(val.as_str());
                    }
                }
                repl.start_completion = true;
                return Ok(ReplControlFlow::Continue);
            }
            completion::CompletionSelection::Interactive(items, query) => {
                // Return control flow to run interactive completion (Skim)
                let query = query.unwrap_or_default();
                let candidates = items;
                return Ok(ReplControlFlow::RunInteractive(Box::new(move || {
                    use crate::completion::framework::SkimCompletionFramework;
                    let result = SkimCompletionFramework::run_with_skim(candidates, Some(query));
                    Ok(result.map(|text| completion_action(text, replacement_range, removal_len)))
                })));
            }
            completion::CompletionSelection::None => {
                // Fallthrough to legacy completion below
            }
        }
    }

    // If no candidates from integrated engine, fall back to legacy completion system
    debug!("No candidates from IntegratedCompletionEngine, falling back to legacy completion");
    let completion_result = completion::input_completion(
        &repl.input,
        repl,
        completion_query,
        &prompt_text,
        &input_text,
    )
    .await;

    // Process the completion result
    let mut completion_handled = false;
    match completion_result {
        completion::CompletionSelection::Selected(val) => {
            debug!("Completion selected: '{}'", val);
            // For history candidates (indicated by clock emoji), replace entire input
            let is_history_candidate = val.starts_with("🕒 ");
            if is_history_candidate {
                let command = val[3..].trim(); // Remove the clock emoji and any extra spaces
                repl.input.reset(command.to_string());
            } else {
                // For regular completions, replace the query part with the selected value
                if let Some(len) = removal_len {
                    repl.input.backspacen(len); // Remove the original query text
                }
                repl.input.insert_str(val.as_str()); // Insert the completion
            }
            completion_handled = true;
        }
        completion::CompletionSelection::Interactive(items, query) => {
            let query = query.unwrap_or_default();
            return Ok(ReplControlFlow::RunInteractive(Box::new(move || {
                use crate::completion::framework::SkimCompletionFramework;
                let result = SkimCompletionFramework::run_with_skim(items, Some(query));
                Ok(
                    result.map(|text| crate::repl::state::InteractiveAction::Patch {
                        text,
                        backspace_count: removal_len.unwrap_or(0),
                    }),
                )
            })));
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
    repl.start_completion = true;
    Ok(ReplControlFlow::Continue)
}

fn completion_action(
    text: String,
    replacement_range: Option<CompletionReplacementRange>,
    removal_len: Option<usize>,
) -> crate::repl::state::InteractiveAction {
    if let Some(range) = replacement_range {
        crate::repl::state::InteractiveAction::ReplaceRange {
            start: range.start,
            end: range.end,
            text,
        }
    } else {
        crate::repl::state::InteractiveAction::Patch {
            text,
            backspace_count: removal_len.unwrap_or(0),
        }
    }
}

fn completion_query_for_input(input: &Input) -> Option<String> {
    match input.get_cursor_word() {
        Ok(Some((_rule, span))) => Some(span.as_str().to_string()),
        _ => input.get_completion_word_fallback(),
    }
}

fn range_text(input: &str, range: CompletionReplacementRange) -> String {
    input
        .chars()
        .skip(range.start)
        .take(range.end.saturating_sub(range.start))
        .collect()
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

    #[test]
    fn integrated_replacement_range_creates_replace_range_action() {
        let action = completion_action(
            "dev-cluster".to_string(),
            Some(CompletionReplacementRange { start: 18, end: 20 }),
            Some(12),
        );

        assert!(matches!(
            action,
            InteractiveAction::ReplaceRange {
                start: 18,
                end: 20,
                text
            } if text == "dev-cluster"
        ));
    }

    #[test]
    fn legacy_completion_creates_patch_action() {
        let action = completion_action("status".to_string(), None, Some(2));

        assert!(matches!(
            action,
            InteractiveAction::Patch {
                backspace_count: 2,
                text
            } if text == "status"
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
}
