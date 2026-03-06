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
    if repl.ai_service.is_some() && !repl.input.is_empty() {
        let input_str = repl.input.as_str().to_string();

        // Clear any existing explanation so the new one takes precedence
        repl.current_ai_explanation = None;
        repl.pending_ai_explanation_input = Some(input_str.clone());

        let ai_tx = repl.ai_tx.clone();
        let service = repl.ai_service.clone().unwrap();

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
    // We need to call execute. Since we are in AI module, we can depend on execution module?
    // Or we returns checks in the main handler.
    // For now, let's just delegate to the main execution handler which we can import.
    super::execution::handle_execute(repl).await?;
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_ai_diagnose(repl: &mut Repl<'_>) -> Result<()> {
    let mut renderer = TerminalRenderer::new();

    if repl.ai_service.is_none() {
        queue!(renderer, Print("\r\n⚠️ AI service is not configured. Set OPENAI_API_KEY or configure dsh to use AI features.\r\n")).ok();
        renderer.flush().ok();
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
        return Ok(());
    }

    if repl.last_status == 0 {
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

    let command = repl.last_command_string.clone();
    let output = repl
        .shell
        .environment
        .read()
        .output_history
        .get_stderr(1)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let exit_code = repl.last_status;

    if let Some(service) = &repl.ai_service {
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
