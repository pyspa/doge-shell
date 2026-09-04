use crate::ai_features::ui::{AiChatUi, DiagnosticContext};
use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::style::Print;
use crossterm::{cursor, queue};

pub(crate) async fn handle_force_ai_suggestion(repl: &mut Repl<'_>) {
    let mut renderer = TerminalRenderer::new();
    queue!(renderer, Print(" 🤖 Generating...\r"), cursor::Hide).ok();
    renderer.flush().ok();
    repl.force_ai_suggestion().await;
}

pub(crate) async fn handle_ai_explain_command(repl: &mut Repl<'_>) {
    // Only proceed if we have an AI service configured and the input is not empty
    if repl.services.ai.is_some() && !repl.input.is_empty() {
        let input_str = repl.input.as_str().to_string();

        // Clear any existing explanation so the new one takes precedence
        repl.ai_ui.current_ai_explanation = None;
        repl.ai_ui.pending_ai_explanation_input = Some(input_str.clone());

        let ai_tx = repl.ai_ui.ai_tx.clone();
        let service = repl.services.ai.clone().unwrap();

        tokio::spawn(async move {
            match crate::ai_features::explain_command_inline(service.as_ref(), &input_str).await {
                Ok(explanation) => {
                    let _ = ai_tx.send(crate::repl::AiEvent::CommandExplanation {
                        input: input_str,
                        explanation,
                    });
                }
                Err(e) => {
                    tracing::debug!("Failed to get AI explanation on demand: {}", e);
                    let _ = ai_tx
                        .send(crate::repl::AiEvent::CommandExplanationError { input: input_str });
                }
            }
        });
    }
}

pub(crate) async fn handle_ai_smart_commit(repl: &mut Repl<'_>) -> Result<ReplControlFlow> {
    // Replace Smart Git Commit logic with "aic" command execution
    repl.input.reset("aic".to_string());
    repl.ai_ui.current_ai_explanation = None;
    repl.ai_ui.pending_ai_explanation_input = None;
    repl.ai_ui.last_explanation = None;
    Ok(ReplControlFlow::ExecuteCurrentInput)
}

pub(crate) fn handle_ai_watch_current_input(repl: &mut Repl<'_>) {
    if let Some(next) = crate::repl::ai_watch::wrap_current_input(repl.input.as_str()) {
        repl.input.reset(next);
        repl.ai_ui.current_ai_explanation = None;
        repl.ai_ui.pending_ai_explanation_input = None;
        repl.ai_ui.last_explanation = None;
        repl.ai_ui.suggestion_manager.clear();
    }
}

pub(crate) async fn handle_ai_diagnose(repl: &mut Repl<'_>) -> Result<()> {
    let mut renderer = TerminalRenderer::new();

    if repl.services.ai.is_none() {
        queue!(
            renderer,
            Print(format!(
                "\r\n⚠️ AI service is not configured. {}\r\n",
                dsh_openai::API_KEY_SETUP_HINT
            ))
        )
        .ok();
        renderer.flush().ok();
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
        return Ok(());
    }

    if repl.state.last_status == 0 {
        queue!(
            renderer,
            Print("\r\n💡 The previous command succeeded (exit code 0). No error to diagnose.\r\n")
        )
        .ok();
        renderer.flush().ok();
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
        return Ok(());
    }

    // Reading only stderr here left every build tool that reports on stdout
    // diagnosed from an empty string.
    let hint_command = repl.state.last_command_string.clone();
    let hint_status = repl.state.last_status;
    let failure = crate::ai_features::resolve_last_failure(
        &repl.shell.environment.read(),
        Some((hint_command.as_str(), hint_status)),
    );
    let (command, output, exit_code) = match failure {
        Some(failure) => (failure.command, failure.output, failure.exit_code),
        None => (hint_command, String::new(), hint_status),
    };

    if let Some(service) = &repl.services.ai {
        let context = DiagnosticContext {
            command: command.clone(),
            output: output.clone(),
            exit_code,
        };

        queue!(renderer, Print("\r\n🔍 Diagnosing error...\r\n")).ok();
        renderer.flush().ok();

        match crate::ai_features::diagnose_output_with_history(
            service.as_ref(),
            &command,
            &output,
            exit_code,
        )
        .await
        {
            Ok((initial_diagnosis, mut history)) => {
                let mut current_diagnosis = initial_diagnosis;

                loop {
                    let mut ui = AiChatUi::new(context.clone(), current_diagnosis.clone());
                    match ui.run() {
                        Ok(crate::ai_features::ui::UiOutcome::ApplyCommand(cmd)) => {
                            repl.input.reset(cmd);
                            break;
                        }
                        Ok(crate::ai_features::ui::UiOutcome::Ask(query)) => {
                            // Print a loading message in the normal alternate screen or terminal
                            // Since ui.run() drops the TerminalGuard, we are back in raw mode but not alt screen?
                            // TerminalGuard restores stdout so it disables alt screen.
                            // We should just print a loading message on the main screen,
                            // or ideally we could retain the alt screen for loading, but for simplicity:
                            let mut tmp_renderer = TerminalRenderer::new();
                            queue!(tmp_renderer, Print("\r\n 🤖 Thinking...\r\n")).ok();
                            tmp_renderer.flush().ok();

                            match crate::ai_features::send_followup_question(
                                service.as_ref(),
                                &mut history,
                                &query,
                            )
                            .await
                            {
                                Ok(new_diagnosis) => {
                                    current_diagnosis = new_diagnosis;
                                    // Loop will re-enter UiChatUi and alternate screen
                                }
                                Err(e) => {
                                    queue!(
                                        tmp_renderer,
                                        Print(format!("❌ Chat failed: {}\r\n", e))
                                    )
                                    .ok();
                                    tmp_renderer.flush().ok();
                                    break;
                                }
                            }
                        }
                        Ok(crate::ai_features::ui::UiOutcome::Quit) => {
                            break;
                        }
                        Err(e) => {
                            let mut err_renderer = TerminalRenderer::new();
                            queue!(err_renderer, Print(format!("❌ UI Error: {}\r\n", e))).ok();
                            err_renderer.flush().ok();
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let mut err_renderer = TerminalRenderer::new();
                queue!(
                    err_renderer,
                    Print(format!("❌ Diagnosis failed: {}\r\n", e))
                )
                .ok();
                err_renderer.flush().ok();
            }
        }
    }

    let mut final_renderer = TerminalRenderer::new();
    final_renderer.flush().ok();
    repl.print_prompt(&mut final_renderer);
    final_renderer.flush().ok();

    Ok(())
}
