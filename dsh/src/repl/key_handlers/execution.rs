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
use dsh_types::command_block::{AiWatchSummary, CommandBlock};
use dsh_types::observed_output::{ObservedOutput, ObservedOutputSnapshot};
use dsh_types::output_history::OutputEntry;
use std::time::{Instant, SystemTime};
use tracing::{debug, warn};

const COMMAND_BLOCK_MAX_OBSERVED_BYTES: usize = 1024 * 1024;

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

    let ai_watch_request = match crate::repl::ai_watch::parse_ai_watch(repl.input.as_str()) {
        Ok(request) => request,
        Err(err) => {
            let mut renderer = TerminalRenderer::new();
            queue!(renderer, Print(format!("\r\nai-watch: {err}\r\n"))).ok();
            repl.print_prompt(&mut renderer);
            repl.print_input(&mut renderer, true, true);
            renderer.flush().ok();
            return Ok(());
        }
    };

    if ai_watch_request.is_some() && repl.ai_service.is_none() {
        let mut renderer = TerminalRenderer::new();
        queue!(
            renderer,
            Print(
                "\r\nai-watch: AI service is not configured. Set AI_CHAT_API_KEY or OPENAI_API_KEY.\r\n"
            )
        )
        .ok();
        repl.print_prompt(&mut renderer);
        repl.print_input(&mut renderer, true, true);
        renderer.flush().ok();
        return Ok(());
    }

    // Handle abbreviation expansion on Enter if cursor is at end of a word
    if ai_watch_request.is_none()
        && let Some(word) = repl.input.get_current_word_for_abbr()
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
        let original_input = repl.input.to_string();
        let input_str = ai_watch_request
            .as_ref()
            .map(|request| request.command.clone())
            .unwrap_or_else(|| original_input.clone());

        execute_shell_command(repl, input_str, ai_watch_request).await?;

        while let Some(command) = repl.shell.pop_requested_eval_command() {
            print!("\r\nblocks rerun: {command}\r\n");
            let _drain_guard = repl.shell.begin_pending_eval_drain()?;
            execute_shell_command(repl, command, None).await?;
        }
    }

    if repl.prompt.read().has_git_root() {
        repl.prompt.read().trigger_git_check();
    }

    // After command execution, show new prompt
    let mut renderer = TerminalRenderer::new();
    repl.print_block_separator(&mut renderer);
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    Ok(())
}

async fn execute_shell_command(
    repl: &mut Repl<'_>,
    input_str: String,
    ai_watch_request: Option<crate::repl::ai_watch::AiWatchRequest>,
) -> Result<()> {
    let start_time = Instant::now();
    let command_timestamp = SystemTime::now();
    repl.last_command_string = input_str.clone();
    repl.completion.clear();
    let output_start_id = repl.shell.environment.read().output_history.latest_id();
    let cwd = std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let shell_tmode = match get_tmode_safe(&repl.tmode) {
        Ok(tmode) => tmode,
        Err(e) => {
            warn!("Cannot get terminal mode: {}", e);
            display_user_error(
                &anyhow::anyhow!("terminal initialization error: {}", e),
                false,
            );

            // Command failed to start due to terminal init error
            print!("\x1b]133;D;1\x1b\\");

            // Show new prompt and skip command execution
            let mut renderer = TerminalRenderer::new();
            repl.print_block_separator(&mut renderer);
            repl.print_prompt(&mut renderer);
            renderer.flush().ok();
            return Ok(());
        }
    };
    let mut ctx = Context::new(repl.shell.pid, repl.shell.pgid, Some(shell_tmode), true);
    let output_observer = Some(ObservedOutput::shared(COMMAND_BLOCK_MAX_OBSERVED_BYTES));
    ctx.output_observer = output_observer.clone();

    // OSC 133 C: Command executed / Output start
    print!("\x1b]133;C\x1b\\");

    let exit_code = match repl
        .shell
        .eval_str(&mut ctx, input_str.clone(), false)
        .await
    {
        Ok(code) => {
            debug!("exit: {} : {:?}", input_str, code);
            repl.last_status = code;
            code
        }
        Err(err) => {
            display_user_error(&err, false);
            repl.last_status = 1;
            1
        }
    };

    // OSC 133 D: Command finished
    print!("\x1b]133;D;{}\x1b\\", exit_code);

    repl.cache.invalidate();

    // Record command timing statistics
    let elapsed = start_time.elapsed();
    if let Some(cmd_name) = command_timing::extract_command_name(&input_str) {
        let mut timing = repl.command_timing.write();
        timing.record(&cmd_name, exit_code, elapsed);
        if let Some(path) = command_timing::get_timing_file_path()
            && let Err(e) = timing.save_to_file_if_due(&path)
        {
            debug!("Failed to save command timing: {}", e);
        }
    }

    repl.shell
        .record_history_outcome(&input_str, exit_code, elapsed);

    let output_entries = repl
        .shell
        .environment
        .read()
        .output_history
        .entries_after_id(output_start_id);
    let observed_output = output_observer.as_ref().and_then(snapshot_observed_output);
    let watched_output = observed_output
        .as_ref()
        .filter(|output| !output.is_empty())
        .map(combined_observed_output)
        .unwrap_or_else(|| combined_output(&output_entries));

    let watch_summary = if let Some(request) = ai_watch_request.as_ref() {
        summarize_ai_watch(
            repl,
            request,
            &input_str,
            &watched_output,
            exit_code,
            elapsed,
        )
        .await
    } else {
        None
    };

    let mut block = CommandBlock::new(
        input_str.clone(),
        cwd,
        exit_code,
        elapsed.as_millis() as u64,
        &output_entries,
        watch_summary,
    );
    if let Some(output) = observed_output.as_ref()
        && !output.is_empty()
    {
        apply_observed_output_to_block(&mut block, output);
    }
    block.timestamp = command_timestamp;
    repl.shell.environment.write().command_blocks.push(block);

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

    Ok(())
}

async fn summarize_ai_watch(
    repl: &mut Repl<'_>,
    request: &crate::repl::ai_watch::AiWatchRequest,
    command: &str,
    output: &str,
    exit_code: i32,
    elapsed: std::time::Duration,
) -> Option<AiWatchSummary> {
    let service = repl.ai_service.clone()?;
    let status = if exit_code == 0 {
        "completed"
    } else {
        "failed"
    }
    .to_string();

    let mut renderer = TerminalRenderer::new();
    queue!(
        renderer,
        Print("\r\nai-watch: analyzing command output...\r\n")
    )
    .ok();
    renderer.flush().ok();

    match crate::ai_features::summarize_watch(
        service.as_ref(),
        command,
        request.goal.as_deref(),
        output,
        exit_code,
        elapsed.as_millis() as u64,
    )
    .await
    {
        Ok(response) => {
            queue!(renderer, Print(format!("ai-watch:\r\n{}\r\n", response))).ok();
            renderer.flush().ok();
            Some(AiWatchSummary::new(request.goal.clone(), status, response))
        }
        Err(err) => {
            let message = format!("analysis failed: {err}");
            queue!(renderer, Print(format!("ai-watch: {message}\r\n"))).ok();
            renderer.flush().ok();
            Some(AiWatchSummary {
                goal: request.goal.clone(),
                status: "analysis-failed".to_string(),
                notes: vec![message],
                suggested_commands: Vec::new(),
                raw_response: None,
            })
        }
    }
}

fn combined_output(entries: &[OutputEntry]) -> String {
    if entries.is_empty() {
        return "(no output captured)".to_string();
    }

    entries
        .iter()
        .map(|entry| {
            let mut text = String::new();
            if !entry.stdout.is_empty() {
                text.push_str(&entry.stdout);
            }
            if !entry.stderr.is_empty() {
                if !text.is_empty() {
                    text.push_str("\n--- STDERR ---\n");
                }
                text.push_str(&entry.stderr);
            }
            text
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn snapshot_observed_output(
    observer: &dsh_types::observed_output::SharedOutputObserver,
) -> Option<ObservedOutputSnapshot> {
    observer.lock().ok().map(|observer| observer.snapshot())
}

fn combined_observed_output(output: &ObservedOutputSnapshot) -> String {
    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&console::strip_ansi_codes(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push_str("\n--- STDERR ---\n");
        }
        text.push_str(&console::strip_ansi_codes(&output.stderr));
    }
    if text.is_empty() {
        "(no output captured)".to_string()
    } else {
        text
    }
}

fn apply_observed_output_to_block(block: &mut CommandBlock, output: &ObservedOutputSnapshot) {
    block.stdout = console::strip_ansi_codes(&output.stdout).to_string();
    block.stderr = console::strip_ansi_codes(&output.stderr).to_string();
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

                // Command failed to start due to terminal init error
                print!("\x1b]133;D;1\x1b\\");

                let mut renderer = TerminalRenderer::new();
                repl.print_block_separator(&mut renderer);
                repl.print_prompt(&mut renderer);
                renderer.flush().ok();
                return Ok(());
            }
        };
        let mut ctx = Context::new(repl.shell.pid, repl.shell.pgid, Some(shell_tmode), true);

        // OSC 133 C: Command executed / Output start
        print!("\x1b]133;C\x1b\\");

        let exit_code = match repl.shell.eval_str(&mut ctx, input.clone(), true).await {
            Ok(code) => {
                repl.last_status = code;
                code
            }
            Err(err) => {
                display_user_error(&err, false);
                repl.last_status = 1;
                1
            }
        };

        // OSC 133 D: Command finished
        print!("\x1b]133;D;{}\x1b\\", exit_code);

        repl.cache.invalidate();
        repl.input.clear();
        repl.suggestion_manager.clear();
        let elapsed = start_time.elapsed();
        repl.shell
            .record_history_outcome(&input, exit_code, elapsed);
        repl.last_duration = Some(elapsed);
    }

    if repl.prompt.read().has_git_root() {
        repl.prompt.read().trigger_git_check();
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

        // OSC 133 D: Command finished (interrupted)
        // 130 is the standard exit code for SIGINT
        print!("\x1b]133;D;130\x1b\\");

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
