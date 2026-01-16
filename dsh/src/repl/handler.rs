use crate::command_palette::CommandPalette;
use crate::command_timing;
use crate::completion;
use crate::completion::MAX_RESULT;
use crate::completion::integrated::CompletionResult;
use crate::errors::display_user_error;
// Was missing? repl: &mut Repl might imply Input is visible via Repl, but Repl struct has input field.
use crate::repl::Repl;
use crate::repl::key_action::{KeyAction, KeyContext, determine_key_action};
use crate::repl::render_transient_prompt_to;
use crate::repl::state::{ShellEvent, SuggestionAcceptMode};
use crate::terminal::renderer::TerminalRenderer;
use crate::utils::editor::open_editor;
use anyhow::Result;
use arboard::Clipboard;
use crossterm::cursor;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use dsh_types::Context;
use nix::sys::termios::Termios;
use std::io::Write;
use std::time::Instant;
use tracing::{debug, warn};

const CTRL: KeyModifiers = KeyModifiers::CONTROL;

/// Safely get Termios, avoiding panic on TTY access failure.
/// Returns Ok(Termios) if successful, Err with descriptive message otherwise.
fn get_tmode_safe(stored_tmode: &Option<Termios>) -> anyhow::Result<Termios> {
    if let Some(tmode) = stored_tmode {
        return Ok(tmode.clone());
    }

    use nix::fcntl::{OFlag, open};
    use nix::sys::stat::Mode;
    use nix::sys::termios::tcgetattr;

    warn!("No stored terminal mode available, attempting to get from /dev/tty");

    let tty_fd = open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
        .map_err(|e| anyhow::anyhow!("Cannot open /dev/tty: {}", e))?;

    tcgetattr(tty_fd).map_err(|e| anyhow::anyhow!("Cannot get terminal attributes: {}", e))
}

pub(crate) async fn check_background_jobs(repl: &mut Repl<'_>, output: bool) -> Result<()> {
    let jobs = repl.shell.check_job_state().await?;
    let exists = !jobs.is_empty();

    if output && exists {
        // Process background output for completed jobs
        for mut job in jobs {
            if !job.foreground {
                job.check_background_all_output().await?;
            }
        }

        // Batch all output operations with a single terminal renderer
        let mut renderer = TerminalRenderer::new();
        let mut output_buffer = String::new();

        // Check remaining jobs in wait_jobs for status messages
        // Note: Completed jobs are no longer in repl.shell.wait_jobs since they were removed
        // by check_job_state, so we only need to check the remaining active jobs.
        for job in &repl.shell.wait_jobs {
            if !job.foreground && output {
                output_buffer.push_str(&format!(
                    "\rdsh: job {} '{}' {}\n",
                    job.job_id, job.cmd, job.state
                ));
            }
        }

        if !output_buffer.is_empty() {
            renderer.write_all(output_buffer.as_bytes())?;
            repl.print_prompt(&mut renderer);
            renderer.flush()?;
        }
    }
    Ok(())
}
pub(crate) async fn handle_event(repl: &mut Repl<'_>, ev: ShellEvent) -> Result<()> {
    match ev {
        ShellEvent::Input(input) => {
            match input {
                Event::Key(key) => repl.handle_key_event(&key).await?,
                Event::Paste(text) => repl.handle_paste_event(&text).await?,
                _ => {}
            }
            Ok(())
        }
        ShellEvent::Paste(text) => {
            repl.handle_paste_event(&text).await?;
            Ok(())
        }
        ShellEvent::ScreenResized => {
            let screen_size = crossterm::terminal::size().unwrap_or_else(|e| {
                warn!(
                    "Failed to get terminal size on resize: {}, keeping current size",
                    e
                );
                (repl.columns as u16, repl.lines as u16)
            });
            repl.columns = screen_size.0 as usize;
            repl.lines = screen_size.1 as usize;
            Ok(())
        }
    }
}

pub(crate) async fn handle_paste_event(repl: &mut Repl<'_>, text: &str) -> Result<()> {
    // Safe Paste: normalize newlines and insert into buffer without execution
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    // We replace newlines with spaces or just keep them if the input supports multiline?
    // Typically shells replace internal newlines with separate commands or just insert them.
    // For safety, we insert as-is. The user sees the newlines and must press Enter to execute.
    // If the text ends with newline, we should probably trim it to avoid accidental execution?
    // But the user might WANT to paste and run.
    // Safe Paste means we put it in the buffer. Use insert_str.
    repl.input.insert_str(&normalized);
    let mut renderer = TerminalRenderer::new();
    repl.print_input(&mut renderer, true, true);
    renderer.flush().ok();
    Ok(())
}
async fn handle_trigger_completion(repl: &mut Repl<'_>) -> Result<bool> {
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
                    return Ok(true);
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
                return Ok(true);
            }
            Err(e) => {
                warn!("Generative command expansion failed: {}", e);
            }
        }
    }

    // Extract the current word at cursor position for completion query
    let completion_query_owned = match repl.input.get_cursor_word() {
        Ok(Some((_rule, span))) => Some(span.as_str().to_string()),
        _ => repl.input.get_completion_word_fallback(),
    };
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
        // If integrated engine returned candidates, show them with skim selector
        let completion_candidates: Vec<completion::Candidate> =
            repl.integrated_completion.to_candidates(engine_candidates);

        let res = completion::select_completion_items_with_framework(
            completion_candidates,
            completion_query,
            &prompt_text,
            &input_text,
            crate::completion::CompletionConfig::default(),
            completion_framework,
        );

        // If result is Some, update input.
        if let Some(val) = res {
            debug!("Completion selected: '{}'", val);
            // For history candidates (indicated by clock emoji), replace entire input
            let is_history_candidate = val.starts_with("🕒 ");
            if is_history_candidate {
                let command = val[3..].trim();
                repl.input.reset(command.to_string());
            } else {
                if let Some(len) = removal_len {
                    repl.input.backspacen(len);
                }
                repl.input.insert_str(val.as_str());
            }
        }

        // If candidates existed, we consider completion handled (either UI showed or selection made).
        // We do NOT fallback to suggestion here.
        repl.start_completion = true;
        return Ok(true);
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
    if let Some(val) = completion_result {
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
    } else {
        // No standard completion selected (Legacy also failed).

        // Fallback: If we have an active suggestion, accept the next word of it.
        if repl.accept_suggestion(SuggestionAcceptMode::Word) {
            debug!("No standard completion, accepted suggestion word fallback");
            completion_handled = true;
        } else {
            debug!("No completion selected and no suggestion to accept");
        }
    }

    // Force a redraw after completion to update the display
    repl.start_completion = true;
    Ok(completion_handled)
}

async fn handle_execute(repl: &mut Repl<'_>) -> Result<()> {
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
    // After command execution, show new prompt
    let mut renderer = TerminalRenderer::new();
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    Ok(())
}

async fn handle_execute_background(repl: &mut Repl<'_>) -> Result<()> {
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
    // After command execution, show new prompt
    let mut renderer = TerminalRenderer::new();
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    Ok(())
}

fn apply_history_item(input: &mut crate::input::Input, item: &dsh_frecency::ItemStats) {
    let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
        .match_index
        .iter()
        .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
        .collect();
    input.reset_with_color_ranges(item.item.clone(), color_ranges);
}

fn handle_history_previous(repl: &mut Repl<'_>) {
    // If completion menu is active, use it for navigation
    if repl.completion.completion_mode() {
        if let Some(item) = repl.completion.backward() {
            apply_history_item(&mut repl.input, item);
        }
        return;
    }

    // Magic Up Arrow: Prefix-based history search
    if let Some(history_arc) = &repl.shell.cmd_history {
        // Try to lock history (non-blocking)
        if let Some(mut history) = history_arc.try_lock() {
            let input_str = repl.input.as_str().to_string();

            // If we are at the start of history navigation (bottom), initialize search
            // Use at_end() to check if we are at the "newest" position
            if history.at_end() && history.search_word.is_none() && !input_str.is_empty() {
                history.search_word = Some(input_str);
            }

            if let Some(cmd) = history.back() {
                repl.input.reset(cmd);
            }
        }
    }
}

fn handle_history_next(repl: &mut Repl<'_>) {
    if repl.completion.completion_mode() {
        if let Some(item) = repl.completion.forward() {
            apply_history_item(&mut repl.input, item);
        }
        return;
    }

    // Magic Down Arrow
    if let Some(history_arc) = &repl.shell.cmd_history
        && let Some(mut history) = history_arc.try_lock()
    {
        // If already at end, we can't go forward
        if history.at_end() {
            return;
        }

        if let Some(cmd) = history.forward() {
            repl.input.reset(cmd);
        } else {
            // If forward() returns None, we are back at the prompt line (future)
            // Restore the original search prefix or clear input
            let saved_input = history.search_word.clone().unwrap_or_default();
            repl.input.reset(saved_input);

            // Ensure index is reset to end (forward should have done it implicitly by failing loop?)
            // Actually history.forward() logic prevents incrementing past limit if logic is strict,
            // but checking `at_end()` handles it.
            // If forward returned None, it means we are now at `len()`.
            // Verify history.rs forward implementation:
            // if len-1 > current_index ...
            // If current_index was len-1, it didn't increment.
            // It returned None.
            // So current_index STAYS at len-1?
            // Wait.
            // In history.rs:
            // if self.histories.len() - 1 > self.current_index { ... } else { None }
            // This prevents `current_index` from ever becoming `len` via `forward`.
            // This is a BUG in my understanding or the history implementation?
            // If `at_end()` is true (`current_index == len`), `back()` decrements to `len-1`.
            // If I am at `len-1` (most recent), `forward()` fails.
            // So I can never get back to `len` (empty prompt) using `forward()`?

            // FIX: manually reset index if we are at the last item and trying to go forward.
            // Check if we are at the last regular entry
            if history.at_latest_entry() {
                history.reset_index();
                let saved_input = history.search_word.clone().unwrap_or_default();
                repl.input.reset(saved_input);
            }
        }
    }
}

async fn handle_ai_diagnose(repl: &mut Repl<'_>) -> Result<()> {
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

fn handle_interrupt(repl: &mut Repl<'_>) -> Result<()> {
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
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
        repl.input.clear();
        repl.multiline_buffer.clear();
        repl.suggestion_manager.clear();
        repl.stop_history_mode();
        Ok(())
    }
}

pub(crate) async fn handle_key_event(repl: &mut Repl<'_>, ev: &KeyEvent) -> Result<()> {
    // DEBUG: Log all key events to trace the issue
    debug!(
        "KEY_EVENT_RECEIVED: code={:?}, modifiers={:?}, kind={:?}",
        ev.code, ev.modifiers, ev.kind
    );

    let redraw = true;
    let mut reset_completion = false;
    // compute previous and new cursor display positions for relative move
    let prompt_w = repl.prompt_mark_width;
    // compute once per event to avoid duplicate width computation
    let prev_cursor_disp = prompt_w + repl.input.cursor_display_width();

    // Reset Ctrl+C state on any key input other than Ctrl+C
    if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
        repl.ctrl_c_state.reset();
    }

    // Handle Ctrl-x prefix
    if matches!((ev.code, ev.modifiers), (KeyCode::Char('x'), CTRL)) {
        repl.ctrl_x_pressed = true;
        return Ok(());
    }

    // If Ctrl-x was pressed, check for secondary key
    if repl.ctrl_x_pressed {
        repl.ctrl_x_pressed = false; // Reset state
        if matches!((ev.code, ev.modifiers), (KeyCode::Char('e'), CTRL)) {
            // Ctrl-x Ctrl-e detected
            match open_editor(repl.input.as_str(), "sh") {
                Ok(content) => {
                    repl.input.reset(content);
                    let mut renderer = TerminalRenderer::new();
                    // Clear screen or just reprint prompts?
                    // Editor usually clears screen or uses alternate screen.
                    // We just need to ensure we are in a clean state.
                    // queue!(renderer, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
                    repl.print_prompt(&mut renderer);
                    repl.print_input(&mut renderer, true, true);
                    renderer.flush()?;
                    return Ok(());
                }
                Err(e) => {
                    warn!("Failed to open editor: {}", e);
                    return Ok(());
                }
            }
        }
        // If not Ctrl-e, ignore Ctrl-x (or maybe treat as regular key if we supported nested)
        // For now, other Ctrl-x sequences are not supported, so we just fall through
        // but since we consumed Ctrl-x, we might want to handle it differently?
        // Emacs usually complains "C-x <key> covers undefined key..."
        // We'll just fall through to normal processing for the current key.
    }

    // --- KeyAction-based dispatch for simple actions ---
    // Build KeyContext from current state
    // Build KeyContext from current state
    let ctx = KeyContext {
        cursor_at_end: repl.input.cursor() == repl.input.len(),
        input_empty: repl.input.is_empty(),
        has_suggestion: repl.suggestion_manager.active.is_some()
            || (repl.input.is_empty() && repl.auto_fix_suggestion.is_some()),
        has_completion: repl.input.completion.is_some(),
        completion_mode: repl.completion.completion_mode(),
        cursor_at_start: repl.input.cursor() == 0,
        next_char: repl.input.char_at(repl.input.cursor()),
        auto_pair: repl.input_preferences.auto_pair,
    };

    // Determine action using pure function
    let action = determine_key_action(ev, &ctx);

    // Handle actions
    match action {
        KeyAction::CursorToBegin => {
            repl.input.move_to_begin();
            // Handle cursor-only movement without full redraw
            let new_cursor_disp = prompt_w + repl.input.cursor_display_width();
            let mut renderer = TerminalRenderer::new();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_cursor_disp);
            renderer.flush().ok();
            return Ok(());
        }
        KeyAction::CursorToEnd => {
            repl.input.move_to_end();
            let new_cursor_disp = prompt_w + repl.input.cursor_display_width();
            let mut renderer = TerminalRenderer::new();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_cursor_disp);
            renderer.flush().ok();
            return Ok(());
        }
        KeyAction::DeleteWordBackward => {
            repl.input.delete_word_backward();
            reset_completion = true;
        }
        KeyAction::DeleteToEnd => {
            repl.input.delete_to_end();
            reset_completion = true;
        }
        KeyAction::DeleteToBeginning => {
            repl.input.delete_to_beginning();
            reset_completion = true;
        }
        KeyAction::HistoryPrevious => {
            handle_history_previous(repl);
        }
        KeyAction::HistoryNext => {
            handle_history_next(repl);
        }
        KeyAction::HistorySearch => {
            repl.select_history();
        }
        KeyAction::AcceptSuggestionWord => {
            if repl.accept_suggestion(SuggestionAcceptMode::Word) {
                reset_completion = true;
            }
        }
        KeyAction::AcceptSuggestionFull => {
            if repl.input.is_empty() && repl.auto_fix_suggestion.is_some() {
                if let Some(fix) = repl.auto_fix_suggestion.take() {
                    repl.input.reset(fix);
                    repl.refresh_inline_suggestion(); // clear potential other suggestions
                    reset_completion = true;
                }
            } else if repl.accept_active_suggestion() {
                repl.completion.clear();
                reset_completion = true;
            }
        }
        KeyAction::RotateSuggestionForward => {
            if repl.suggestion_manager.rotate(1) {
                reset_completion = false;
            }
        }
        KeyAction::RotateSuggestionBackward => {
            if repl.suggestion_manager.rotate(-1) {
                reset_completion = false;
            }
        }
        KeyAction::CursorLeft => {
            if repl.input.cursor() > 0 {
                repl.input.completion = None;
                repl.input.move_by(-1);
                repl.completion.clear();

                let mut renderer = TerminalRenderer::new();
                let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
                repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
                queue!(renderer, cursor::Show).ok();
                renderer.flush().ok();
                return Ok(());
            } else {
                return Ok(());
            }
        }
        KeyAction::CursorRight => {
            if repl.input.cursor() < repl.input.len() {
                repl.input.move_by(1);
                repl.completion.clear();

                let mut renderer = TerminalRenderer::new();
                let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
                repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
                queue!(renderer, cursor::Show).ok();
                renderer.flush().ok();
                return Ok(());
            } else {
                return Ok(());
            }
        }
        KeyAction::CursorWordLeft => {
            repl.input.move_word_left();
            repl.completion.clear();

            let mut renderer = TerminalRenderer::new();
            let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
            queue!(renderer, cursor::Show).ok();
            renderer.flush().ok();
            return Ok(());
        }
        KeyAction::CursorWordRight => {
            repl.input.move_word_right();
            repl.completion.clear();

            let mut renderer = TerminalRenderer::new();
            let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
            queue!(renderer, cursor::Show).ok();
            renderer.flush().ok();
            return Ok(());
        }
        KeyAction::ExpandAbbreviationAndInsertSpace => {
            if let Some(word) = repl.input.get_current_word_for_abbr()
                && let Some(expansion) = repl.shell.environment.read().abbreviations.get(&word)
            {
                let expansion = expansion.clone();
                if repl.input.replace_current_word(&expansion) {
                    reset_completion = true;
                }
            }

            repl.input.insert(' ');
            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        KeyAction::InsertPairedChar { open, close } => {
            repl.input.insert(open);
            repl.input.insert(close);
            repl.input.move_by(-1);

            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        KeyAction::OvertypeClosingBracket(_ch) => {
            repl.input.move_by(1);

            let mut renderer = TerminalRenderer::new();
            let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
            if let Err(e) = queue!(renderer, cursor::Show) {
                warn!("Failed to show cursor: {}", e);
            }
            if let Err(e) = renderer.flush() {
                warn!("Failed to flush renderer: {}", e);
            }
            return Ok(());
        }
        KeyAction::InsertChar(ch) => {
            repl.input.insert(ch);
            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        KeyAction::Backspace => {
            let cursor = repl.input.cursor();
            if repl.input_preferences.auto_pair && cursor > 0 && cursor < repl.input.len() {
                let prev_char = repl.input.char_at(cursor - 1);
                let next_char = repl.input.char_at(cursor);

                if let (Some(p), Some(n)) = (prev_char, next_char) {
                    let pairs = [('(', ')'), ('{', '}'), ('[', ']'), ('\'', '\''), ('"', '"')];
                    if pairs.iter().any(|(o, c)| *o == p && *c == n) {
                        repl.input.delete_char();
                    }
                }
            }

            reset_completion = true;
            repl.input.backspace();
            repl.completion.clear();
            repl.input.color_ranges = None;
        }
        KeyAction::AiAutoFix => {
            repl.trigger_auto_fix();
        }
        KeyAction::AiSmartCommit => {
            // Replace Smart Git Commit logic with "aic" command execution
            repl.input.reset("aic".to_string());
            handle_execute(repl).await?;
            return Ok(());
        }

        KeyAction::AiDiagnose => {
            return handle_ai_diagnose(repl).await;
        }
        KeyAction::ForceAiSuggestion => {
            let mut renderer = TerminalRenderer::new();
            queue!(renderer, Print(" 🤖 Generating...\r"), cursor::Hide).ok();
            renderer.flush().ok();
            repl.force_ai_suggestion().await;
        }
        KeyAction::TriggerCompletion => {
            if handle_trigger_completion(repl).await? {
                reset_completion = true;
            }
        }
        KeyAction::Execute => {
            handle_execute(repl).await?;
            return Ok(());
        }
        KeyAction::ExecuteBackground => {
            handle_execute_background(repl).await?;
            return Ok(());
        }
        KeyAction::OpenCommandPalette => {
            // Disable raw mode so Skim can handle terminal state correctly
            disable_raw_mode().ok();

            CommandPalette::run(repl.shell, repl.input.as_str())?;

            // Re-enable raw mode for the shell
            enable_raw_mode().ok();

            let mut renderer = TerminalRenderer::new();
            repl.print_prompt(&mut renderer);
            renderer.flush().ok();
            return Ok(());
        }
        KeyAction::AcceptCompletion => {
            if let Some(comp) = &repl.input.completion.take() {
                repl.input.reset(comp.to_string());
            }
            repl.completion.clear();
        }
        KeyAction::Interrupt => {
            return handle_interrupt(repl);
        }
        KeyAction::ClearScreen => {
            let mut renderer = TerminalRenderer::new();
            queue!(renderer, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
            repl.print_prompt(&mut renderer);
            renderer.flush().ok();
            repl.input.clear();
            repl.suggestion_manager.clear();
            return Ok(());
        }
        KeyAction::Paste => {
            if let Ok(mut clipboard) = Clipboard::new()
                && let Ok(content) = clipboard.get_text()
            {
                repl.input.insert_str(&content);
                repl.completion.clear();
            }
        }
        KeyAction::OpenEditor => {
            // Already handled via Ctrl-x state check, or unimplemented via key action dispatch
        }
        KeyAction::ToggleSudo => {
            if repl.esc_state.on_pressed() {
                repl.toggle_sudo().await?;
                repl.esc_state.reset();
            }
            return Ok(());
        }
        KeyAction::CancelCompletion => {
            if repl.input.completion.is_some() || repl.suggestion_manager.active.is_some() {
                repl.completion.clear();
                repl.suggestion_manager.clear();
                let mut renderer = TerminalRenderer::new();
                repl.print_input(&mut renderer, true, true);
                renderer.flush().ok();
            }
        }
        KeyAction::Unsupported => {
            warn!("unsupported key event: {:?}", ev);
        }
    }

    if redraw {
        let mut renderer = TerminalRenderer::new();
        repl.print_input(&mut renderer, reset_completion, true);
        renderer.flush().ok();
    }
    // Note: For cursor-only movements (redraw=false), cursor positioning
    // is handled directly in the key event handlers to avoid full redraw
    Ok(())
}
