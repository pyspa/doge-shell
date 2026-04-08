use crate::repl::Repl;
use crate::repl::key_action::{KeyAction, KeyContext, determine_key_action};
use crate::repl::state::{ReplControlFlow, ShellEvent};
use crate::terminal::renderer::TerminalRenderer;
use crate::utils::editor::open_editor;
use anyhow::Result;
use arboard::Clipboard;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use nix::sys::termios::Termios;
use tracing::{debug, warn};

// Import granular handlers
use super::key_handlers::*;

const CTRL: KeyModifiers = KeyModifiers::CONTROL;

/// Safely get Termios, avoiding panic on TTY access failure.
/// Returns Ok(Termios) if successful, Err with descriptive message otherwise.
pub(crate) fn get_tmode_safe(stored_tmode: &Option<Termios>) -> anyhow::Result<Termios> {
    if let Some(tmode) = stored_tmode {
        return Ok(tmode.clone());
    }

    use nix::fcntl::{OFlag, open};
    use nix::sys::stat::Mode;
    use nix::sys::termios::tcgetattr;

    warn!("No stored terminal mode available, attempting to get from /dev/tty");

    let tty_fd = open("/dev/tty", OFlag::O_RDONLY, Mode::empty())
        .map_err(|e| anyhow::anyhow!("Cannot open /dev/tty: {}", e))?;

    tcgetattr(&tty_fd).map_err(|e| anyhow::anyhow!("Cannot get terminal attributes: {}", e))
}

pub(crate) async fn handle_event(repl: &mut Repl<'_>, ev: ShellEvent) -> Result<ReplControlFlow> {
    match ev {
        ShellEvent::Input(input) => match input {
            Event::Key(key) => repl.handle_key_event(&key).await,
            Event::Paste(text) => {
                editing::handle_paste_event(repl, &text).await?;
                Ok(ReplControlFlow::Continue)
            }
            _ => Ok(ReplControlFlow::Continue),
        },
        ShellEvent::Paste(text) => {
            editing::handle_paste_event(repl, &text).await?;
            Ok(ReplControlFlow::Continue)
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
            Ok(ReplControlFlow::Continue)
        }
    }
}

pub(crate) async fn handle_key_event(
    repl: &mut Repl<'_>,
    ev: &KeyEvent,
) -> Result<ReplControlFlow> {
    // DEBUG: Log all key events to trace the issue
    debug!(
        "KEY_EVENT_RECEIVED: code={:?}, modifiers={:?}, kind={:?}",
        ev.code, ev.modifiers, ev.kind
    );

    let redraw = true;
    let mut reset_completion = false;
    let _prompt_w = repl.prompt_mark_width;

    // Reset Ctrl+C state on any key input other than Ctrl+C
    if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
        repl.ctrl_c_state.reset();
    }

    // Handle Ctrl-x prefix
    if matches!((ev.code, ev.modifiers), (KeyCode::Char('x'), CTRL)) {
        repl.ctrl_x_pressed = true;
        return Ok(ReplControlFlow::Continue);
    }

    // If Ctrl-x was pressed, check for secondary key
    if repl.ctrl_x_pressed {
        repl.ctrl_x_pressed = false; // Reset state
        if matches!((ev.code, ev.modifiers), (KeyCode::Char('e'), CTRL)) {
            // Ctrl-x Ctrl-e detected
            match open_editor(repl.input.as_str(), "sh") {
                Ok(content) => {
                    repl.input.reset(content);
                    repl.last_input_change_time = std::time::Instant::now();
                    repl.current_ai_explanation = None;

                    let mut renderer = TerminalRenderer::new();
                    repl.print_prompt(&mut renderer);
                    repl.print_input(&mut renderer, true, true);
                    renderer.flush()?;
                    return Ok(ReplControlFlow::Continue);
                }
                Err(e) => {
                    warn!("Failed to open editor: {}", e);
                    return Ok(ReplControlFlow::Continue);
                }
            }
        }
    }

    // --- KeyAction-based dispatch for simple actions ---
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
        KeyAction::MacroRecord => {
            auxiliary::handle_macro_record(repl).await?;
        }
        KeyAction::CursorToBegin => {
            return navigation::handle_cursor_to_begin(repl, 0).await;
        }
        KeyAction::CursorToEnd => {
            return navigation::handle_cursor_to_end(repl, 0).await;
        }
        KeyAction::DeleteWordBackward => {
            reset_completion = editing::handle_delete_word_backward(repl);
        }
        KeyAction::DeleteToEnd => {
            reset_completion = editing::handle_delete_to_end(repl);
        }
        KeyAction::DeleteToBeginning => {
            reset_completion = editing::handle_delete_to_beginning(repl);
        }
        KeyAction::HistoryPrevious => {
            navigation::handle_history_previous(repl);
        }
        KeyAction::HistoryNext => {
            navigation::handle_history_next(repl);
        }
        KeyAction::HistorySearch => {
            return repl.select_history();
        }
        KeyAction::AcceptSuggestionWord => {
            reset_completion = completion::handle_accept_suggestion_word(repl);
        }
        KeyAction::AcceptSuggestionFull => {
            reset_completion = completion::handle_accept_suggestion_full(repl);
        }
        KeyAction::RotateSuggestionForward => {
            reset_completion = completion::handle_rotate_suggestion_forward(repl);
        }
        KeyAction::RotateSuggestionBackward => {
            reset_completion = completion::handle_rotate_suggestion_backward(repl);
        }
        KeyAction::CursorLeft => {
            return navigation::handle_cursor_left(repl, 0).await;
        }
        KeyAction::CursorRight => {
            return navigation::handle_cursor_right(repl, 0).await;
        }
        KeyAction::CursorWordLeft => {
            return navigation::handle_cursor_word_left(repl, 0).await;
        }
        KeyAction::CursorWordRight => {
            return navigation::handle_cursor_word_right(repl, 0).await;
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
            editing::handle_insert_paired_char(repl, open, close);
        }
        KeyAction::OvertypeClosingBracket(_ch) => {
            return editing::handle_overtype_closing_bracket(repl, 0).await;
        }
        KeyAction::InsertChar(ch) => {
            editing::handle_insert_char(repl, ch);
        }
        KeyAction::Backspace => {
            reset_completion = editing::handle_backspace(repl);
        }
        KeyAction::AiAutoFix => {
            repl.trigger_auto_fix();
        }
        KeyAction::AiSmartCommit => {
            return ai::handle_ai_smart_commit(repl).await;
        }
        KeyAction::AiDiagnose => {
            ai::handle_ai_diagnose(repl).await?;
            return Ok(ReplControlFlow::Continue);
        }
        KeyAction::ForceAiSuggestion => {
            ai::handle_force_ai_suggestion(repl).await;
        }
        KeyAction::AiExplainCommand => {
            ai::handle_ai_explain_command(repl).await;
        }
        KeyAction::TriggerCompletion => match completion::handle_trigger_completion(repl).await? {
            ReplControlFlow::Continue => {
                reset_completion = true;
            }
            ReplControlFlow::RunInteractive(f) => {
                return Ok(ReplControlFlow::RunInteractive(f));
            }
            control_flow => {
                return Ok(control_flow);
            }
        },
        KeyAction::Execute => {
            repl.current_ai_explanation = None;
            repl.pending_ai_explanation_input = None;
            repl.last_explanation = None;
            return Ok(ReplControlFlow::ExecuteCurrentInput);
        }
        KeyAction::ExecuteBackground => {
            execution::handle_execute_background(repl).await?;
            return Ok(ReplControlFlow::Continue);
        }
        KeyAction::OpenCommandPalette => {
            return Ok(ReplControlFlow::OpenCommandPalette);
        }
        KeyAction::AcceptCompletion => {
            completion::handle_accept_completion(repl);
        }
        KeyAction::Interrupt => {
            execution::handle_interrupt(repl)?;
            return Ok(ReplControlFlow::Continue);
        }
        KeyAction::ClearScreen => {
            return auxiliary::handle_clear_screen(repl);
        }
        KeyAction::Paste => {
            if let Ok(mut clipboard) = Clipboard::new()
                && let Ok(content) = clipboard.get_text()
            {
                editing::handle_paste_event(repl, &content).await?;
                // repl.input.insert_str(&content); // handle_paste_event does this + normalize
                // repl.completion.clear(); // handled in handle_paste_event?
                // handle_paste_event calls replace, but logic says safe paste.
                // editing::handle_paste_event implements safe paste.
            }
        }
        KeyAction::OpenEditor => {
            // Already handled via Ctrl-x state check
        }
        KeyAction::ToggleSudo => {
            if repl.esc_state.on_pressed() {
                repl.toggle_sudo().await?;
                repl.esc_state.reset();
            }
            return Ok(ReplControlFlow::Continue);
        }
        KeyAction::CancelCompletion => {
            completion::handle_cancel_completion(repl);
        }
        KeyAction::Unsupported => {
            warn!("unsupported key event: {:?}", ev);
        }
    }

    // Determine if input was likely modified by the action.
    // Reset AI explanation state when input changes so a fresh explanation
    // will be requested after the next idle period.
    if matches!(
        action,
        KeyAction::InsertChar(_)
            | KeyAction::Backspace
            | KeyAction::DeleteWordBackward
            | KeyAction::DeleteToEnd
            | KeyAction::DeleteToBeginning
            | KeyAction::AcceptSuggestionWord
            | KeyAction::AcceptSuggestionFull
            | KeyAction::AcceptCompletion
            | KeyAction::ExpandAbbreviationAndInsertSpace
            | KeyAction::InsertPairedChar { .. }
            | KeyAction::OvertypeClosingBracket(_)
            | KeyAction::Paste
            | KeyAction::HistoryPrevious
            | KeyAction::HistoryNext
            | KeyAction::HistorySearch
    ) {
        repl.last_input_change_time = std::time::Instant::now();
        repl.current_ai_explanation = None;
        repl.pending_ai_explanation_input = None;
    }

    // On execute or interrupt, clear explanation state and erase the explanation line
    if matches!(
        action,
        KeyAction::Execute | KeyAction::ExecuteBackground | KeyAction::Interrupt
    ) {
        repl.current_ai_explanation = None;
        repl.pending_ai_explanation_input = None;
        repl.last_explanation = None;
    }

    if redraw {
        let mut renderer = TerminalRenderer::new();
        repl.print_input(&mut renderer, reset_completion, true);
        renderer.flush().ok();
    }
    // Note: For cursor-only movements (redraw=false), cursor positioning
    // is handled directly in the key event handlers to avoid full redraw
    Ok(ReplControlFlow::Continue)
}
