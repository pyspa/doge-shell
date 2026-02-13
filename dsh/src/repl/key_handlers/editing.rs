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
    if repl.completion.is_changed(repl.input.as_str()) {
        repl.completion.clear();
    }
}

/// Handle backspace. Returns true if completion should be reset.
pub(crate) fn handle_backspace(repl: &mut Repl<'_>) -> bool {
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

    repl.input.backspace();
    repl.completion.clear();
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

    if repl.completion.is_changed(repl.input.as_str()) {
        repl.completion.clear();
    }
}

pub(crate) async fn handle_overtype_closing_bracket(
    repl: &mut Repl<'_>,
    prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
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
