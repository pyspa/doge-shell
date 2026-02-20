use crate::ai_features::ui::{AiChatUi, DiagnosticContext};
use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::style::Print;
use crossterm::{cursor, queue};

/// Handle force AI suggestion.
pub(crate) async fn handle_force_ai_suggestion(repl: &mut Repl<'_>) {
    let mut renderer = TerminalRenderer::new();
    queue!(renderer, Print(" 🤖 Generating...\r"), cursor::Hide).ok();
    renderer.flush().ok();
    repl.force_ai_suggestion().await;
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

        match crate::ai_features::diagnose_output(service.as_ref(), &command, &output, exit_code)
            .await
        {
            Ok(diagnosis) => {
                let mut ui = AiChatUi::new(context, diagnosis);
                if let Err(e) = ui.run() {
                    let mut err_renderer = TerminalRenderer::new();
                    queue!(err_renderer, Print(format!("❌ UI Error: {}\r\n", e))).ok();
                    err_renderer.flush().ok();
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
