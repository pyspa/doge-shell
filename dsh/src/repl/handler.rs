use crate::command_timing;
use crate::completion;
use crate::completion::MAX_RESULT;
use crate::completion::integrated::CompletionResult;
use crate::errors::display_user_error;
// Was missing? repl: &mut Repl might imply Input is visible via Repl, but Repl struct has input field.
use crate::repl::Repl;
use crate::repl::render_transient_prompt_to;
use crate::repl::state::{ShellEvent, SuggestionAcceptMode};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use arboard::Clipboard;
use crossterm::cursor;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use dsh_types::Context;
use nix::sys::termios::Termios;
use std::io::Write;
use std::time::Instant;
use tracing::{debug, warn};

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

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
    let cursor = repl.input.cursor();

    // Reset Ctrl+C state on any key input other than Ctrl+C
    if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
        repl.ctrl_c_state.reset();
    }

    match (ev.code, ev.modifiers) {
        // history
        (KeyCode::Up, NONE) => {
            if repl.completion.completion_mode() {
                if let Some(item) = repl.completion.backward() {
                    // Convert match_index to color_ranges (for backward compatibility, assume all matches are commands that exist)
                    let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
                        .match_index
                        .iter()
                        .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
                        .collect();
                    repl.input
                        .reset_with_color_ranges(item.item.clone(), color_ranges);
                }
            } else {
                repl.set_completions();
                if let Some(item) = repl.completion.backward() {
                    // Convert match_index to color_ranges (for backward compatibility, assume all matches are commands that exist)
                    let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
                        .match_index
                        .iter()
                        .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
                        .collect();
                    repl.input
                        .reset_with_color_ranges(item.item.clone(), color_ranges);
                }
            }
        }
        // history
        (KeyCode::Down, NONE) => {
            if repl.completion.completion_mode()
                && let Some(item) = repl.completion.forward()
            {
                // Convert match_index to color_ranges (for backward compatibility, assume all matches are commands that exist)
                let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
                    .match_index
                    .iter()
                    .map(|&idx| (idx, idx + 1, crate::input::ColorType::CommandExists))
                    .collect();
                repl.input
                    .reset_with_color_ranges(item.item.clone(), color_ranges);
            }
        }

        (KeyCode::Right, modifiers)
            if modifiers.contains(CTRL)
                && repl.suggestion_manager.active.is_some()
                && repl.input.completion.is_none()
                && repl.input.cursor() == repl.input.len() =>
        {
            if repl.accept_suggestion(SuggestionAcceptMode::Word) {
                reset_completion = true;
            }
        }
        (KeyCode::Char('f'), ALT)
            if repl.suggestion_manager.active.is_some()
                && repl.input.completion.is_none()
                && repl.input.cursor() == repl.input.len() =>
        {
            if repl.accept_suggestion(SuggestionAcceptMode::Word) {
                reset_completion = true;
            }
        }
        (KeyCode::Char(']'), ALT) => {
            if repl.suggestion_manager.rotate(1) {
                reset_completion = false;
            }
        }
        (KeyCode::Char('['), ALT) => {
            if repl.suggestion_manager.rotate(-1) {
                reset_completion = false;
            }
        }
        (KeyCode::Left, modifiers) if !modifiers.contains(CTRL) => {
            if repl.input.cursor() > 0 {
                repl.input.completion = None;
                repl.input.move_by(-1);
                repl.completion.clear();

                // Move cursor relatively, ensure cursor is visible in fast path
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
        (KeyCode::Right, modifiers)
            if repl.suggestion_manager.active.is_some()
                && repl.input.completion.is_none()
                && repl.input.cursor() == repl.input.len()
                && !modifiers.contains(CTRL) =>
        {
            if repl.accept_active_suggestion() {
                repl.completion.clear();
                reset_completion = true;
            }
        }
        (KeyCode::Right, modifiers)
            if repl.input.completion.is_some() && !modifiers.contains(CTRL) =>
        {
            // TODO refactor
            if let Some(completion) = &repl.input.completion {
                let completion_chars = completion.chars().count();

                if cursor >= completion_chars {
                    return Ok(());
                }

                let suffix_byte_index = completion
                    .char_indices()
                    .nth(cursor)
                    .map(|(idx, _)| idx)
                    .unwrap_or_else(|| completion.len());

                if suffix_byte_index >= completion.len() {
                    return Ok(());
                }

                let suffix = &completion[suffix_byte_index..];

                if let Some((fragment, post)) = suffix.split_once(' ') {
                    let mut new_input = repl.input.as_str().to_owned();
                    new_input.push_str(fragment);
                    if !post.is_empty() {
                        new_input.push(' ');
                    }
                    repl.input.reset(new_input);
                } else {
                    repl.input.reset(completion.to_string());
                    repl.input.completion = None;
                }
            }
            repl.completion.clear();
        }
        (KeyCode::Right, modifiers) if !modifiers.contains(CTRL) => {
            if repl.input.cursor() < repl.input.len() {
                repl.input.move_by(1);
                repl.completion.clear();

                // Move cursor relatively, ensure cursor is visible in fast path
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
        (KeyCode::Char('f'), CTRL)
            if repl.suggestion_manager.active.is_some()
                && repl.input.completion.is_none()
                && repl.input.cursor() == repl.input.len() =>
        {
            if repl.accept_active_suggestion() {
                repl.completion.clear();
                reset_completion = true;
            }
        }
        (KeyCode::Char(' '), NONE) => {
            // Handle abbreviation expansion before inserting space
            if let Some(word) = repl.input.get_current_word_for_abbr() {
                // debug!("ABBR_EXPANSION: Found word for expansion: '{}'", word);
                if let Some(expansion) = repl.shell.environment.read().abbreviations.get(&word) {
                    // debug!(
                    //     "ABBR_EXPANSION: Found expansion for '{}': '{}'",
                    //     word, expansion
                    // );
                    let expansion = expansion.clone();
                    if repl.input.replace_current_word(&expansion) {
                        // debug!(
                        //     "ABBR_EXPANSION: Successfully replaced '{}' with '{}'",
                        //     word, expansion
                        // );
                        // Abbreviation was expanded, force redraw
                        reset_completion = true;
                    } else {
                        // debug!("ABBR_EXPANSION: Failed to replace word '{}'", word);
                    }
                } else {
                    // debug!("ABBR_EXPANSION: No expansion found for word '{}'", word);
                    let _abbrs = repl.shell.environment.read().abbreviations.clone();
                    // debug!("ABBR_EXPANSION: Available abbreviations: {:?}", _abbrs);
                }
            } else {
                // debug!("ABBR_EXPANSION: No word found for expansion at cursor position");
            }

            repl.input.insert(' ');
            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        (KeyCode::Char(ch), NONE) if matches!(ch, '(' | '{' | '[' | '\'' | '"') => {
            // Auto-pairing logic
            let closing = match ch {
                '(' => ')',
                '{' => '}',
                '[' => ']',
                '\'' => '\'',
                '"' => '"',
                _ => ch, // Should not happen due to guard
            };

            repl.input.insert(ch);
            repl.input.insert(closing);
            repl.input.move_by(-1);

            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        (KeyCode::Char(ch), NONE) if matches!(ch, ')' | '}' | ']' | '\'' | '"') => {
            // Overtype logic
            let current_input = repl.input.as_str();
            let cursor = repl.input.cursor();

            if cursor < current_input.len() {
                // Safe access to next char
                if let Some(next_char) = current_input[cursor..].chars().next()
                    && next_char == ch
                {
                    repl.input.move_by(1);

                    // Move cursor relatively
                    let mut renderer = TerminalRenderer::new();
                    let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
                    let prompt_w = repl.prompt_mark_width;
                    let prev_cursor_disp = prompt_w + repl.input.cursor_display_width() - 1; // Approx
                    repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
                    if let Err(e) = queue!(renderer, cursor::Show) {
                        warn!("Failed to show cursor: {}", e);
                    }
                    if let Err(e) = renderer.flush() {
                        warn!("Failed to flush renderer: {}", e);
                    }
                    return Ok(());
                }
            }

            repl.input.insert(ch);
            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        (KeyCode::Char(ch), NONE) => {
            repl.input.insert(ch);
            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        (KeyCode::Char(ch), SHIFT) => {
            repl.input.insert(ch);
            if repl.completion.is_changed(repl.input.as_str()) {
                repl.completion.clear();
            }
        }
        (KeyCode::Backspace, NONE) => {
            // Auto-unpairing logic
            let cursor = repl.input.cursor();
            if cursor > 0 && cursor < repl.input.len() {
                let prev_char = repl.input.char_at(cursor - 1);
                let next_char = repl.input.char_at(cursor);

                if let (Some(p), Some(n)) = (prev_char, next_char) {
                    let pairs = [('(', ')'), ('{', '}'), ('[', ']'), ('\'', '\''), ('"', '"')];
                    if pairs.iter().any(|(o, c)| *o == p && *c == n) {
                        repl.input.delete_char(); // Delete closing char
                    }
                }
            }

            reset_completion = true;
            repl.input.backspace();
            repl.completion.clear();
            repl.input.color_ranges = None;
        }
        // Auto-Fix (Alt+f)
        (KeyCode::Char('f'), ALT) => {
            repl.perform_auto_fix().await;
        }
        // Smart Git Commit (Alt+c)
        (KeyCode::Char('c'), ALT) => {
            if repl.ai_service.is_some() {
                // Check if git is available and there are staged changes
                let output = std::process::Command::new("git")
                    .args(["diff", "--cached"])
                    .output();

                match output {
                    Ok(output) if output.status.success() => {
                        let diff = String::from_utf8_lossy(&output.stdout).to_string();
                        if !diff.trim().is_empty() {
                            // Show "processing"
                            let mut renderer = TerminalRenderer::new();
                            queue!(renderer, Print(" 🤖 Generating..."), cursor::Hide).ok();
                            renderer.flush().ok();

                            repl.perform_smart_commit_logic(&diff).await;
                        } else {
                            // No staged changes
                            // Maybe warn user? For now just do nothing or maybe flash?
                        }
                    }
                    _ => {
                        // Git failed or not found
                    }
                }
            }
        }
        // AI Quick Actions (Alt+a)
        (KeyCode::Char('a'), ALT) => {
            repl.show_ai_quick_actions().await?;
            return Ok(());
        }
        // Error Diagnosis (Alt+d)
        (KeyCode::Char('d'), ALT) => {
            if repl.ai_service.is_some() && repl.last_status != 0 {
                let mut renderer = TerminalRenderer::new();
                queue!(renderer, Print("\r\n🔍 Diagnosing error...\r\n")).ok();
                renderer.flush().ok();

                // Get the last command and output for diagnosis
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
            return Ok(());
        }
        (KeyCode::Tab, NONE) | (KeyCode::BackTab, NONE) => {
            // Check for Smart Pipe Expansion (|? query)
            // We check if the cursor is at the end of a block starting with |?
            if let Some(smart_pipe_query) = repl.detect_smart_pipe() {
                match repl.expand_smart_pipe(smart_pipe_query).await {
                    Ok(expanded) => {
                        // Replace |? ... with expanded command
                        // We need to find where |? starts logic again or just use the detection result
                        // The detect_smart_pipe should ideally return range too.
                        // For now let's re-find it or simplify.
                        // detect_smart_pipe returns the query string.
                        // We assume the cursor is at the end.
                        // We assume usage pattern: `cmd |? something<TAB>`

                        // Remove the query and "|? " prefix
                        // Query length + 3 ("|? ")

                        // Careful with whitespace.
                        // Simple Approach: Find the last "|?" and replace from there.

                        let input_str = repl.input.as_str();
                        if let Some(idx) = input_str.rfind("|?") {
                            let prefix = &input_str[..idx]; // keep "|?" out? No we want to replace "|? ..."
                            let mut new_input = prefix.to_string();
                            new_input.push_str("| ");
                            new_input.push_str(&expanded);
                            repl.input.reset(new_input);
                            repl.completion.clear();
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        warn!("Smart pipe expansion failed: {}", e);
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
            for (i, candidate) in engine_candidates.iter().enumerate() {
                debug!("Integrated engine candidate {}: {:?}", i, candidate);
            }

            // Attempt to get completion result
            // First try with integrated completion engine, then fall back to legacy system
            let completion_result = if !engine_candidates.is_empty() {
                // If integrated engine returned candidates, show them with skim selector
                let completion_candidates: Vec<completion::Candidate> =
                    repl.integrated_completion.to_candidates(engine_candidates);

                debug!(
                    "Converted to {} UI candidates for {:?}",
                    completion_candidates.len(),
                    completion_framework
                );
                for (i, candidate) in completion_candidates.iter().enumerate() {
                    debug!("Skim UI candidate {}: {:?}", i, candidate);
                }

                completion::select_completion_items_with_framework(
                    completion_candidates,
                    completion_query,
                    &prompt_text,
                    &input_text,
                    crate::completion::CompletionConfig::default(),
                    completion_framework,
                )
            } else {
                debug!(
                    "No candidates from IntegratedCompletionEngine, falling back to legacy completion"
                );
                // If no candidates from integrated engine, fall back to legacy completion system
                // This handles path completion, command completion from PATH, etc.
                completion::input_completion(
                    &repl.input,
                    repl,
                    completion_query,
                    &prompt_text,
                    &input_text,
                )
            };

            // Process the completion result
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
                debug!("Input after completion: '{}'", repl.input.to_string());
            } else {
                // No completion was selected - this happens when:
                // 1. No candidates were found (empty candidate list returns None immediately)
                // 2. User cancelled the completion interface (e.g. pressed ESC in skim)
                // 3. User made no selection from the completion list
                debug!("No completion selected");
                // In this case, the input remains unchanged and no error is shown to user
                // This is the "silent failure" behavior when no matches are found
            }

            // Force a redraw after completion to update the display
            reset_completion = true;
            repl.start_completion = true;
            debug!("Set start_completion flag to true and reset_completion to true");

            // Note: When no matches are found, no UI is shown and no error is displayed to user.
            // The integrated completion engine returns an empty vector when no candidates match,
            // which immediately results in a fallback to legacy completion.
            // If legacy completion also finds no matches, completion::input_completion returns None,
            // leading to the "No completion selected" case above.
        }
        // Enter key - also handle Ctrl+J (LF) which may be sent after raw mode restoration
        (KeyCode::Enter, NONE) | (KeyCode::Char('j'), CTRL) => {
            debug!(
                "ENTER_KEY_HANDLER: Starting Enter key processing, input='{}'",
                repl.input.as_str()
            );
            // AI Output Pipe (|!)
            if let Some((command, query)) = repl.detect_ai_pipe() {
                repl.input.clear();
                repl.run_ai_pipe(command, query).await?;
                return Ok(());
            }

            // Generative Command (??)
            if repl.input.as_str().trim_start().starts_with("??") {
                let query = repl.input.as_str().trim_start()[2..].trim().to_string();
                if !query.is_empty() {
                    match repl.run_generative_command(&query).await {
                        Ok(generated) => {
                            repl.input.reset(generated);
                            // Don't execute immediately, let user review
                            return Ok(());
                        }
                        Err(e) => {
                            warn!("Generative command failed: {}", e);
                            // Fall through to normal execution (which will likely fail but that's fine)
                        }
                    }
                }
            }

            // Handle abbreviation expansion on Enter if cursor is at end of a word
            if let Some(word) = repl.input.get_current_word_for_abbr()
                && let Some(expansion) = repl.shell.environment.read().abbreviations.get(&word)
            {
                let expansion = expansion.clone();
                if repl.input.replace_current_word(&expansion) {
                    // Abbreviation was expanded - the input will be redrawn after command execution
                    debug!("Abbreviation '{}' expanded to '{}'", word, expansion);
                }
            }

            repl.input.completion.take();
            repl.stop_history_mode();

            // Transient Prompt Logic
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
                        eprintln!("dsh: terminal initialization error: {}", e);
                        // Show new prompt and skip command execution
                        let mut renderer = TerminalRenderer::new();
                        repl.print_prompt(&mut renderer);
                        renderer.flush().ok();
                        return Ok(());
                    }
                };
                let mut ctx = Context::new(repl.shell.pid, repl.shell.pgid, shell_tmode, true);
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
            return Ok(());
        }
        (KeyCode::Enter, ALT) => {
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
                        eprintln!("dsh: terminal initialization error: {}", e);
                        let mut renderer = TerminalRenderer::new();
                        repl.print_prompt(&mut renderer);
                        renderer.flush().ok();
                        return Ok(());
                    }
                };
                let mut ctx = Context::new(repl.shell.pid, repl.shell.pgid, shell_tmode, true);
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
            return Ok(());
        }
        (KeyCode::Char('a'), CTRL) => {
            repl.input.move_to_begin();
        }
        (KeyCode::Char('e'), CTRL) if repl.input.completion.is_some() => {
            if let Some(comp) = &repl.input.completion.take() {
                repl.input.reset(comp.to_string());
            }
            repl.completion.clear();
        }
        (KeyCode::Char('e'), CTRL) => {
            repl.input.move_to_end();
        }
        (KeyCode::Char('c'), CTRL) => {
            debug!("CTRL_C_HANDLER: Ctrl+C pressed, processing...");
            let mut renderer = TerminalRenderer::new();

            if repl.ctrl_c_state.on_pressed() {
                // Second Ctrl+C - exit shell normally
                // queue message and flush once here
                queue!(renderer, Print("\r\nExiting shell...\r\n")).ok();
                renderer.flush().ok();
                repl.should_exit = true;
                return Ok(());
            } else {
                // First Ctrl+C - reset prompt + show message
                // queue message and defer flushing until after prompt
                queue!(
                    renderer,
                    Print("\r\n(Press Ctrl+C again within 3 seconds to exit)\r\n")
                )
                .ok();
                repl.print_prompt(&mut renderer);
                renderer.flush().ok();
                repl.input.clear();
                repl.suggestion_manager.clear();
                return Ok(());
            }
        }
        (KeyCode::Char('l'), CTRL) => {
            let mut renderer = TerminalRenderer::new();
            queue!(renderer, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
            repl.print_prompt(&mut renderer);
            renderer.flush().ok();
            repl.input.clear();
            repl.suggestion_manager.clear();
            return Ok(());
        }
        (KeyCode::Char('d'), CTRL) => {
            let mut renderer = TerminalRenderer::new();
            queue!(renderer, Print("\r\nuse 'exit' to leave the shell\n")).ok();
            repl.print_prompt(&mut renderer);
            renderer.flush().ok();
            repl.input.clear();
            repl.suggestion_manager.clear();
            return Ok(());
        }
        (KeyCode::Char('r'), CTRL) => {
            repl.select_history();
        }
        (KeyCode::Char('v'), CTRL) => {
            // Paste clipboard content at current cursor position
            if let Ok(mut clipboard) = Clipboard::new()
                && let Ok(content) = clipboard.get_text()
            {
                // Insert the clipboard content at the current cursor position
                repl.input.insert_str(&content);
                repl.completion.clear();
            }
        }
        (KeyCode::Char('w'), CTRL) => {
            repl.input.delete_word_backward();
            repl.completion.clear();
            reset_completion = true;
        }
        (KeyCode::Char('k'), CTRL) => {
            repl.input.delete_to_end();
            repl.completion.clear();
            reset_completion = true;
        }
        (KeyCode::Char('u'), CTRL) => {
            repl.input.delete_to_beginning();
            repl.completion.clear();
            reset_completion = true;
        }
        (KeyCode::Left, modifiers) if modifiers.contains(CTRL) => {
            repl.input.move_word_left();
            repl.completion.clear();

            // Move cursor relatively
            let mut renderer = TerminalRenderer::new();
            let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
            queue!(renderer, cursor::Show).ok();
            renderer.flush().ok();
            return Ok(());
        }
        (KeyCode::Right, modifiers) if modifiers.contains(CTRL) => {
            repl.input.move_word_right();
            repl.completion.clear();

            // Move cursor relatively
            let mut renderer = TerminalRenderer::new();
            let new_disp = repl.prompt_mark_width + repl.input.cursor_display_width();
            repl.move_cursor_relative(&mut renderer, prev_cursor_disp, new_disp);
            queue!(renderer, cursor::Show).ok();
            renderer.flush().ok();
            return Ok(());
        }
        (KeyCode::Esc, NONE) => {
            if repl.esc_state.on_pressed() {
                // Double Esc detected - toggle sudo
                repl.toggle_sudo().await?;
                // Reset state to avoid triple press issues
                repl.esc_state.reset();
            } else {
                // Single Esc - standard behavior is often to clear line or cancel completion?
                // Existing behavior usually falls through to _ (unsupported) or is handled else where?
                // If I consume it here, I must ensure I don't break other things.
                // But Esc is usually handled. Is it?
                // I checked earlier and didn't see explicit Esc handler.
                // If completion is active, Esc should cancel completion.
                if repl.input.completion.is_some() || repl.suggestion_manager.active.is_some() {
                    repl.completion.clear();
                    repl.suggestion_manager.clear();
                    let mut renderer = TerminalRenderer::new();
                    repl.print_input(&mut renderer, true, true);
                    renderer.flush().ok();
                } else {
                    // Maybe clear line? Or do nothing?
                    // Let's do nothing on single, just wait for potential second.
                }
            }
            return Ok(());
        }
        _ => {
            warn!("unsupported key event: {:?}", ev);
        }
    }

    if redraw {
        // debug!("Redrawing input, reset_completion: {}", reset_completion);
        let mut renderer = TerminalRenderer::new();
        repl.print_input(&mut renderer, reset_completion, true);
        renderer.flush().ok();
    }
    // Note: For cursor-only movements (redraw=false), cursor positioning
    // is handled directly in the key event handlers to avoid full redraw
    Ok(())
}
