use crate::repl::Repl;
use crate::repl::key_action::{KeyAction, KeyContext, determine_key_action};
use crate::repl::keybind::{BoundAction, Resolved};
use crate::repl::render;
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
            Event::Resize(cols, rows) => {
                handle_resize(repl, cols, rows);
                Ok(ReplControlFlow::Continue)
            }
            _ => Ok(ReplControlFlow::Continue),
        },
        ShellEvent::Paste(text) => {
            editing::handle_paste_event(repl, &text).await?;
            Ok(ReplControlFlow::Continue)
        }
    }
}

/// Apply a terminal resize.
///
/// Every wrapping calculation (`Input::cursor_pos`, `Input::line_count`,
/// `render_transient_prompt_to`, `render_hint_if_room`) reads `repl.terminal_ui.columns`,
/// which is otherwise only set once in `Repl::setup()`. Without this the whole
/// render path keeps using the pre-resize width.
///
/// Only the input line is redrawn. The terminal has already reflowed the
/// preprompt above it, so re-emitting it would leave the pre-resize copy on
/// screen — one duplicated prompt per resize.
pub(crate) fn handle_resize(repl: &mut Repl<'_>, cols: u16, rows: u16) {
    let cols = cols as usize;
    let rows = rows as usize;
    if cols == repl.terminal_ui.columns && rows == repl.terminal_ui.lines {
        return;
    }

    // The scroll region is expressed in absolute rows, so it has to be torn
    // down at the *old* height and re-established at the new one.
    let mut renderer = TerminalRenderer::new();
    repl.terminal_ui
        .status_line
        .borrow_mut()
        .disarm(&mut renderer);
    renderer.flush().ok();

    repl.terminal_ui.columns = cols;
    repl.terminal_ui.lines = rows;
    repl.terminal_ui
        .status_line
        .borrow_mut()
        .set_size(cols as u16, rows as u16);

    if cols == 0 {
        return;
    }

    repl.refresh_status_line();

    // The recorded cursor row was measured at the old width. The terminal
    // rewrapped the input line, so the row the cursor now sits on is the one
    // implied by the new width — recompute it before `print_input` uses it to
    // move back up to the prompt mark.
    let (_, cursor_y) = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.terminal_ui.last_drawn_cursor_y = cursor_y;

    let mut renderer = TerminalRenderer::new();
    repl.print_input(&mut renderer, false, false);
    if let Err(e) = renderer.flush() {
        warn!("Failed to redraw after resize: {}", e);
    }
}

/// Whether an action makes the snippet placeholder stops meaningless.
///
/// Ordinary editing keeps them (they are re-anchored by the length delta).
/// Listed here are the actions that replace the buffer with unrelated text, or
/// end the line entirely, where a char-offset delta cannot describe what
/// happened.
fn placeholders_invalidated_by(action: &KeyAction) -> bool {
    matches!(
        action,
        KeyAction::Execute
            | KeyAction::ExecuteBackground
            | KeyAction::Interrupt
            | KeyAction::Eof
            | KeyAction::HistoryPrevious
            | KeyAction::HistoryNext
            | KeyAction::HistorySearch
            | KeyAction::Undo
            | KeyAction::Redo
            | KeyAction::OpenEditor
            | KeyAction::OpenBlockBrowser
            | KeyAction::OpenCommandPalette
            | KeyAction::MacroRecord
            | KeyAction::ResumeLastJob
            | KeyAction::ToggleSudo
            | KeyAction::AiAutoFix
            | KeyAction::AiSmartCommit
            | KeyAction::AiDiagnose
            | KeyAction::AiExplainCommand
            | KeyAction::AiWatchCurrentInput
            | KeyAction::ForceAiSuggestion
            | KeyAction::AcceptSuggestionFull
            | KeyAction::AcceptSuggestionWord
            | KeyAction::RotateSuggestionForward
            | KeyAction::RotateSuggestionBackward
    )
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
    let _prompt_w = repl.terminal_ui.prompt_mark_width;

    // Reset Ctrl+C state on any key input other than Ctrl+C
    if !matches!((ev.code, ev.modifiers), (KeyCode::Char('c'), CTRL)) {
        repl.terminal_ui.ctrl_c_state.reset();
    }

    // --- User key bindings, layered in front of the built-in table ---
    //
    // The environment lock is taken and released here: dispatching below needs
    // `&mut repl`, and a Lisp binding re-enters the environment for writing.
    let resolved = {
        let environment = repl.shell.environment.clone();
        let bindings = environment.read();
        bindings
            .variable_state
            .keybindings
            .resolve(&mut repl.pending_chord, ev)
    };

    let bound_action = match resolved {
        Resolved::Pending => return Ok(ReplControlFlow::Continue),
        Resolved::Unbound(sequence) => {
            // A chord that goes nowhere drops the prefix and lets the key that
            // ended it do its normal job — what the hardcoded Ctrl-x handling
            // did before this layer existed.
            //
            // Consuming the key instead would mean an accidental Ctrl-x makes
            // the next Enter silently not run the command, and an accidental
            // Ctrl-x eats the next character typed. Neither is acceptable for a
            // prefix that sits next to Ctrl-c and Ctrl-z.
            debug!("key sequence not bound, falling through: {}", sequence);
            None
        }
        Resolved::Bound(BoundAction::Lisp(function)) => {
            return run_lisp_binding(repl, &function);
        }
        Resolved::Bound(BoundAction::Action(action)) => Some(action),
        Resolved::Fallthrough => None,
    };

    // --- KeyAction-based dispatch for simple actions ---
    let ctx = KeyContext {
        cursor_at_end: repl.input.cursor() == repl.input.len(),
        input_empty: repl.input.is_empty(),
        has_suggestion: repl.ai_ui.suggestion_manager.active.is_some()
            || (repl.input.is_empty() && repl.ai_ui.auto_fix_suggestion.is_some()),
        has_completion: repl.input.completion.is_some(),
        completion_mode: repl.completion_ui.completion.completion_mode(),
        cursor_at_start: repl.input.cursor() == 0,
        next_char: repl.input.char_at(repl.input.cursor()),
        auto_pair: repl.ai_ui.input_preferences.auto_pair,
        multiline_active: !repl.state.multiline_buffer.is_empty(),
    };

    // A user binding wins outright; otherwise fall back to the built-in table.
    let action = match bound_action {
        Some(action) => action,
        None => determine_key_action(ev, &ctx),
    };

    // `Alt+.` is run-scoped: any other action ends the run, so the next press
    // starts over from the newest command. Keyed off the action rather than the
    // raw key so a rebound key keeps working.
    if !matches!(action, KeyAction::InsertLastArgument) {
        repl.state.last_arg = None;
    }

    // Placeholder stops must survive ordinary editing — filling a value in is
    // the whole point — so they are only dropped by actions that replace the
    // line wholesale or leave it. Everything else re-anchors them below.
    if placeholders_invalidated_by(&action) {
        repl.state.placeholders = None;
    }
    let placeholder_anchor = repl
        .state
        .placeholders
        .as_ref()
        .map(|_| (repl.input.cursor(), repl.input.len()));

    // Handle actions
    match action {
        KeyAction::InsertLastArgument => {
            reset_completion = input_shortcuts::handle_insert_last_argument(repl);
        }
        KeyAction::InsertSnippet => {
            let inserted = input_shortcuts::handle_insert_snippet(repl);
            reset_completion = inserted;
            if inserted {
                // The picker painted over the prompt; put it back before the
                // trailing redraw refreshes the input line.
                //
                // `redraw_prompt`, not `print_prompt`: this is the same command
                // line, so emitting a fresh OSC 133 A (with no matching D) and
                // re-running the pre-prompt hooks would open a bogus command
                // block in shell-integration-aware terminals. And it only runs
                // when the picker actually drew — bailing early (no snippets,
                // database unavailable) must leave the screen alone.
                let mut renderer = TerminalRenderer::new();
                render::redraw_prompt(repl, &mut renderer);
                renderer.flush().ok();
            }
        }
        KeyAction::NextPlaceholder => {
            input_shortcuts::handle_placeholder_step(repl, true);
        }
        KeyAction::PrevPlaceholder => {
            input_shortcuts::handle_placeholder_step(repl, false);
        }
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
                && let Some(expansion) = repl
                    .shell
                    .environment
                    .read()
                    .variable_state
                    .abbreviations
                    .get(&word)
            {
                let expansion = expansion.clone();
                if repl.input.replace_current_word(&expansion) {
                    reset_completion = true;
                }
            }

            repl.input.insert(' ');
            if repl
                .completion_ui
                .completion
                .is_changed(repl.input.as_str())
            {
                repl.completion_ui.completion.clear();
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
        KeyAction::DeleteCharForward => {
            reset_completion = editing::handle_delete_char_forward(repl);
        }
        KeyAction::Yank => {
            reset_completion = editing::handle_yank(repl);
        }
        KeyAction::Undo => {
            reset_completion = editing::handle_undo(repl);
        }
        KeyAction::Redo => {
            reset_completion = editing::handle_redo(repl);
        }
        KeyAction::Eof => {
            execution::handle_eof(repl)?;
            return Ok(ReplControlFlow::Continue);
        }
        KeyAction::ResumeLastJob => {
            execution::handle_resume_last_job(repl)?;
            return Ok(ReplControlFlow::Continue);
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
        KeyAction::AiWatchCurrentInput => {
            ai::handle_ai_watch_current_input(repl);
            reset_completion = true;
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
            repl.ai_ui.current_ai_explanation = None;
            repl.ai_ui.pending_ai_explanation_input = None;
            repl.ai_ui.last_explanation = None;
            return Ok(ReplControlFlow::ExecuteCurrentInput);
        }
        KeyAction::ExecuteBackground => {
            execution::handle_execute_background(repl).await?;
            return Ok(ReplControlFlow::Continue);
        }
        KeyAction::OpenCommandPalette => {
            return Ok(ReplControlFlow::OpenCommandPalette);
        }
        KeyAction::OpenBlockBrowser => {
            return auxiliary::handle_open_block_browser(repl);
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
                // repl.completion_ui.completion.clear(); // handled in handle_paste_event?
                // handle_paste_event calls replace, but logic says safe paste.
                // editing::handle_paste_event implements safe paste.
            }
        }
        KeyAction::OpenEditor => {
            // vim/emacs paint the whole screen; hand it over intact.
            let _status_pause = crate::repl::status_line::StatusLinePause::new(
                repl.terminal_ui.status_line.clone(),
            );
            match open_editor(repl.input.as_str(), "sh") {
                Ok(content) => {
                    repl.input.reset(content);
                    repl.ai_ui.last_input_change_time = std::time::Instant::now();
                    repl.ai_ui.current_ai_explanation = None;

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
        KeyAction::ToggleSudo => {
            if repl.terminal_ui.esc_state.on_pressed() {
                repl.toggle_sudo().await?;
                repl.terminal_ui.esc_state.reset();
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

    // Re-anchor the snippet stops against whatever the action did to the
    // buffer. Measuring the length change is enough for insert/delete edits;
    // the actions that rewrite the line arbitrarily were dropped above.
    if let Some((cursor_before, len_before)) = placeholder_anchor
        && let Some(state) = repl.state.placeholders.as_mut()
        && !matches!(
            action,
            KeyAction::NextPlaceholder | KeyAction::PrevPlaceholder
        )
    {
        let delta = repl.input.len() as isize - len_before as isize;
        state.adjust(cursor_before, delta);
    }

    // Determine if input was likely modified by the action.
    // Reset AI explanation state when input changes so a fresh explanation
    // will be requested after the next idle period.
    if matches!(
        action,
        KeyAction::InsertChar(_)
            | KeyAction::Backspace
            | KeyAction::DeleteCharForward
            | KeyAction::DeleteWordBackward
            | KeyAction::DeleteToEnd
            | KeyAction::DeleteToBeginning
            | KeyAction::Yank
            | KeyAction::Undo
            | KeyAction::Redo
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
            | KeyAction::AiWatchCurrentInput
            | KeyAction::InsertLastArgument
            | KeyAction::InsertSnippet
    ) {
        repl.ai_ui.last_input_change_time = std::time::Instant::now();
        repl.ai_ui.current_ai_explanation = None;
        repl.ai_ui.pending_ai_explanation_input = None;
    }

    // On execute or interrupt, clear explanation state and erase the explanation line
    if matches!(
        action,
        KeyAction::Execute | KeyAction::ExecuteBackground | KeyAction::Interrupt
    ) {
        repl.ai_ui.current_ai_explanation = None;
        repl.ai_ui.pending_ai_explanation_input = None;
        repl.ai_ui.last_explanation = None;
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

/// Runs a key bound to a Lisp function.
///
/// The function receives the current input and cursor position. If it returns
/// a string, that string is inserted at the cursor; any other value leaves the
/// buffer alone, which is how a binding does something purely side-effecting.
///
/// The call is synchronous, so a slow function blocks the prompt — the same
/// property Command Palette Lisp actions have.
fn run_lisp_binding(repl: &mut Repl<'_>, function: &str) -> Result<ReplControlFlow> {
    use crate::lisp::Value;

    let args = vec![
        Value::String(repl.input.as_str().to_string()),
        Value::Int(repl.input.cursor() as i64),
    ];

    // The engine is an `Rc<RefCell<..>>` shared with the shell; clone the
    // handle so no borrow is held across the call, which may re-enter the
    // shell environment.
    let engine = std::rc::Rc::clone(&repl.shell.lisp_engine);
    let result = engine.borrow().run_func_values(function, args);

    let mut changed = false;
    match result {
        Ok(Value::String(text)) if !text.is_empty() => {
            repl.input.insert_str(&text);
            changed = true;
        }
        Ok(_) => {}
        Err(err) => {
            // A broken binding must not take the shell down with it.
            warn!("key binding '{}' failed: {}", function, err);
        }
    }

    if changed {
        repl.ai_ui.last_input_change_time = std::time::Instant::now();
        repl.ai_ui.current_ai_explanation = None;
        repl.ai_ui.pending_ai_explanation_input = None;
    }

    let mut renderer = TerminalRenderer::new();
    repl.print_input(&mut renderer, changed, true);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}
