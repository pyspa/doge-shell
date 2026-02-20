use super::super::handler::get_tmode_safe;
use crate::command_timing;
use crate::errors::display_user_error;
use crate::repl::Repl;
use crate::repl::render_transient_prompt_to;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::queue;
use crossterm::style::Print;
use dsh_types::Context;
use std::time::Instant;
use tracing::{debug, warn};

/// Execute the current content of the input buffer.
pub(crate) async fn handle_execute(repl: &mut Repl<'_>) -> Result<()> {
    debug!(
        "ENTER_KEY_HANDLER: Starting Enter key processing, input='{}'",
        repl.input.as_str()
    );

    // Multiline Check
    {
        let current_input = repl.input.as_str().to_string();
        let combined_input = if !repl.multiline_buffer.is_empty() {
            format!("{}{}", repl.multiline_buffer, current_input)
        } else {
            current_input.clone()
        };

        if crate::parser::is_incomplete_input(&combined_input) {
            repl.multiline_buffer.push_str(&current_input);
            repl.multiline_buffer.push('\n');
            repl.input.clear();
            repl.completion.clear();
            repl.suggestion_manager.clear();

            print!("\r\n");
            let mut renderer = TerminalRenderer::new();
            repl.print_prompt(&mut renderer);
            renderer.flush().ok();
            return Ok(());
        } else if !repl.multiline_buffer.is_empty() {
            // Complete!
            repl.input.reset(combined_input);
            repl.multiline_buffer.clear();
        }
    }

    // AI Output Pipe (|!)
    if let Some((command, query)) = repl.detect_ai_pipe() {
        repl.input.clear();
        repl.run_ai_pipe(command, query).await?;
        return Ok(());
    }

    // Handle abbreviation expansion on Enter if cursor is at end of a word
    if let Some(word) = repl.input.get_current_word_for_abbr()
        && let Some(expansion) = repl.shell.environment.read().abbreviations.get(&word)
    {
        let expansion = expansion.clone();
        if repl.input.replace_current_word(&expansion) {
            debug!("Abbreviation '{}' expanded to '{}'", word, expansion);
        }
    }

    repl.input.completion.take();
    repl.stop_history_mode();

    // Transient Prompt Logic
    if repl
        .shell
        .environment
        .read()
        .input_preferences
        .transient_prompt
    {
        use crate::input::display_width;

        let mut stdout = std::io::stdout();
        let input_width = display_width(repl.input.as_str());
        let prompt_width = repl.prompt_mark_width;
        let cols = repl.columns;

        render_transient_prompt_to(
            &mut stdout,
            &repl.input,
            input_width,
            prompt_width,
            cols as u16,
        )
        .ok();
    }

    print!("\r\n");
    if !repl.input.is_empty() {
        let start_time = Instant::now();
        let input_str = repl.input.to_string();
        repl.last_command_string = input_str.clone();
        repl.completion.clear();
        let shell_tmode = match get_tmode_safe(&repl.tmode) {
            Ok(tmode) => tmode,
            Err(e) => {
                warn!("Cannot get terminal mode: {}", e);
                display_user_error(
                    &anyhow::anyhow!("terminal initialization error: {}", e),
                    false,
                );
                // Show new prompt and skip command execution
                let mut renderer = TerminalRenderer::new();
                repl.print_block_separator(&mut renderer);
                repl.print_prompt(&mut renderer);
                renderer.flush().ok();
                return Ok(());
            }
        };
        let mut ctx = Context::new(repl.shell.pid, repl.shell.pgid, Some(shell_tmode), true);
        let exit_code = match repl
            .shell
            .eval_str(&mut ctx, input_str.clone(), false)
            .await
        {
            Ok(code) => {
                debug!("exit: {} : {:?}", repl.input.as_str(), code);
                repl.last_status = code;
                code
            }
            Err(err) => {
                display_user_error(&err, false);
                repl.last_status = 1;
                1
            }
        };

        repl.cache.invalidate();

        // Record command timing statistics
        let elapsed = start_time.elapsed();
        if let Some(cmd_name) = command_timing::extract_command_name(&input_str) {
            let mut timing = repl.command_timing.write();
            timing.record(&cmd_name, exit_code, elapsed);
            // Save immediately for real-time updates
            if let Some(path) = command_timing::get_timing_file_path()
                && let Err(e) = timing.save_to_file(&path)
            {
                debug!("Failed to save command timing: {}", e);
            }
        }

        // Auto-Notify logic
        {
            // Check threshold
            let threshold = repl
                .shell
                .environment
                .read()
                .input_preferences
                .auto_notify_threshold;
            let enabled = repl
                .shell
                .environment
                .read()
                .input_preferences
                .auto_notify_enabled;

            if enabled && elapsed >= std::time::Duration::from_secs(threshold) {
                use notify_rust::Notification;
                let summary = if exit_code == 0 {
                    "Command Completed"
                } else {
                    "Command Failed"
                };
                let cmd_preview = if input_str.len() > 50 {
                    format!("{}...", &input_str[..47])
                } else {
                    input_str.clone()
                };
                let body = format!("'{}' took {:.1}s", cmd_preview, elapsed.as_secs_f64());

                // Fire and forget notification
                if let Err(e) = Notification::new()
                    .summary(summary)
                    .body(&body)
                    .appname("doge-shell")
                    .show()
                {
                    warn!("Failed to send desktop notification: {}", e);
                }
            }
        }

        repl.input.clear();
        repl.suggestion_manager.clear();
        repl.last_command_time = Some(Instant::now());
        repl.last_duration = Some(elapsed);

        // Show error diagnosis hint if auto_diagnose is enabled
        if exit_code != 0
            && repl.ai_service.is_some()
            && repl
                .shell
                .environment
                .read()
                .input_preferences
                .auto_diagnose
        {
            let mut renderer = TerminalRenderer::new();
            queue!(renderer, Print("💡 Press Alt+d to diagnose this error\r\n")).ok();
            renderer.flush().ok();
        }
    }

    // Synchronously refresh git status for accurate display after command execution
    // This ensures the prompt always shows the correct git state immediately
    if repl.prompt.read().has_git_root() {
        repl.prompt.write().refresh_git_status_sync();
    }

    // After command execution, show new prompt
    let mut renderer = TerminalRenderer::new();
    repl.print_block_separator(&mut renderer);
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    Ok(())
}

/// Execute the current content of the input buffer in the background.
pub(crate) async fn handle_execute_background(repl: &mut Repl<'_>) -> Result<()> {
    repl.input.completion.take();
    repl.stop_history_mode();
    print!("\r\n");
    if !repl.input.is_empty() {
        let start_time = Instant::now();
        repl.completion.clear();
        let input = repl.input.to_string();
        repl.last_command_string = input.clone();
        let shell_tmode = match get_tmode_safe(&repl.tmode) {
            Ok(tmode) => tmode,
            Err(e) => {
                warn!("Cannot get terminal mode for background execution: {}", e);
                display_user_error(
                    &anyhow::anyhow!("terminal initialization error: {}", e),
                    false,
                );
                let mut renderer = TerminalRenderer::new();
                repl.print_block_separator(&mut renderer);
                repl.print_prompt(&mut renderer);
                renderer.flush().ok();
                return Ok(());
            }
        };
        let mut ctx = Context::new(repl.shell.pid, repl.shell.pgid, Some(shell_tmode), true);
        match repl.shell.eval_str(&mut ctx, input, true).await {
            Ok(code) => {
                repl.last_status = code;
            }
            Err(err) => {
                display_user_error(&err, false);
                repl.last_status = 1;
            }
        }
        repl.cache.invalidate();
        repl.input.clear();
        repl.suggestion_manager.clear();
        repl.last_duration = Some(start_time.elapsed());
    }

    // Synchronously refresh git status for accurate display after command execution
    if repl.prompt.read().has_git_root() {
        repl.prompt.write().refresh_git_status_sync();
    }

    // After command execution, show new prompt
    let mut renderer = TerminalRenderer::new();
    repl.print_block_separator(&mut renderer);
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    Ok(())
}

/// Handle Ctrl+C interrupt.
pub(crate) fn handle_interrupt(repl: &mut Repl<'_>) -> Result<()> {
    debug!("CTRL_C_HANDLER: Ctrl+C pressed, processing...");
    let mut renderer = TerminalRenderer::new();

    let should_exit = if cfg!(debug_assertions) {
        repl.ctrl_c_state.on_pressed()
    } else {
        false
    };

    if should_exit {
        queue!(renderer, Print("\r\nExiting shell...\r\n")).ok();
        renderer.flush().ok();
        repl.should_exit = true;
        Ok(())
    } else {
        if cfg!(debug_assertions) {
            queue!(
                renderer,
                Print("\r\n(Press Ctrl+C again within 3 seconds to exit)\r\n")
            )
            .ok();
        } else {
            queue!(renderer, Print("\r\n")).ok();
        }
        repl.print_block_separator(&mut renderer);
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
        repl.input.clear();
        repl.multiline_buffer.clear();
        repl.suggestion_manager.clear();
        repl.stop_history_mode();
        Ok(())
    }
}
