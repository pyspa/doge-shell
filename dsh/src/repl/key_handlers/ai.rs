use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use crossterm::style::Print;

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
    if repl.ai_service.is_some() && repl.last_status != 0 {
        let mut renderer = TerminalRenderer::new();
        queue!(renderer, Print("\r\n🔍 Diagnosing error...\r\n")).ok();
        renderer.flush().ok();

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
            match crate::ai_features::diagnose_output(
                service.as_ref(),
                &command,
                &output,
                exit_code,
            )
            .await
            {
                Ok(diagnosis) => {
                    for line in diagnosis.lines() {
                        queue!(renderer, Print(format!("{}\r\n", line))).ok();
                    }
                    queue!(renderer, Print("\r\n")).ok();
                }
                Err(e) => {
                    queue!(renderer, Print(format!("❌ Diagnosis failed: {}\r\n", e))).ok();
                }
            }
        }

        renderer.flush().ok();
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
    }
    Ok(())
}
