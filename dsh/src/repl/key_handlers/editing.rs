use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use tracing::warn;

/// Handle inserting a character.
pub(crate) fn handle_insert_char(repl: &mut Repl<'_>, ch: char) {
    repl.input.insert(ch);
    if repl
        .completion_ui
        .completion
        .is_changed(repl.input.as_str())
    {
        repl.completion_ui.completion.clear();
    }
}

/// Handle backspace. Returns true if completion should be reset.
pub(crate) fn handle_backspace(repl: &mut Repl<'_>) -> bool {
    let cursor = repl.input.cursor();
    if repl.ai_ui.input_preferences.auto_pair && cursor > 0 && cursor < repl.input.len() {
        let prev_char = repl.input.char_at(cursor - 1);
        let next_char = repl.input.char_at(cursor);

        if let (Some(p), Some(n)) = (prev_char, next_char) {
            let pairs = [('(', ')'), ('{', '}'), ('[', ']'), ('\'', '\''), ('"', '"')];
            if pairs.iter().any(|(o, c)| *o == p && *c == n) {
                repl.input.delete_char();
            }
        }
    }

    repl.input.backspace();
    repl.completion_ui.completion.clear();
    repl.input.color_ranges = None;
    true // reset_completion = true
}

/// Re-insert the text removed by the last kill (Ctrl-Y).
pub(crate) fn handle_yank(repl: &mut Repl<'_>) -> bool {
    if !repl.input.yank() {
        return false;
    }
    repl.completion_ui.completion.clear();
    repl.input.color_ranges = None;
    true
}

/// Undo the last edit (Ctrl-_).
pub(crate) fn handle_undo(repl: &mut Repl<'_>) -> bool {
    if !repl.input.undo() {
        return false;
    }
    repl.completion_ui.completion.clear();
    true
}

/// Redo the last undone edit (Alt-/).
pub(crate) fn handle_redo(repl: &mut Repl<'_>) -> bool {
    if !repl.input.redo() {
        return false;
    }
    repl.completion_ui.completion.clear();
    true
}

/// Delete the character under the cursor (Delete key, and Ctrl-D on a
/// non-empty line). `Input::delete_char` is a no-op at end of buffer.
pub(crate) fn handle_delete_char_forward(repl: &mut Repl<'_>) -> bool {
    repl.input.delete_char();
    repl.completion_ui.completion.clear();
    repl.input.color_ranges = None;
    true // reset_completion = true
}

pub(crate) fn handle_delete_word_backward(repl: &mut Repl<'_>) -> bool {
    repl.input.delete_word_backward();
    true
}

pub(crate) fn handle_delete_to_end(repl: &mut Repl<'_>) -> bool {
    repl.input.delete_to_end();
    true
}

pub(crate) fn handle_delete_to_beginning(repl: &mut Repl<'_>) -> bool {
    repl.input.delete_to_beginning();
    true
}

pub(crate) fn handle_insert_paired_char(repl: &mut Repl<'_>, open: char, close: char) {
    repl.input.insert(open);
    repl.input.insert(close);
    repl.input.move_by(-1);

    if repl
        .completion_ui
        .completion
        .is_changed(repl.input.as_str())
    {
        repl.completion_ui.completion.clear();
    }
}

pub(crate) async fn handle_overtype_closing_bracket(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let prev_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.input.move_by(1);
    let new_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
    if let Err(e) = queue!(renderer, cursor::Show) {
        warn!("Failed to show cursor: {}", e);
    }
    if let Err(e) = renderer.flush() {
        warn!("Failed to flush renderer: {}", e);
    }
    Ok(ReplControlFlow::Continue)
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
