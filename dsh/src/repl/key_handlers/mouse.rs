use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

pub(crate) async fn handle_mouse_event(
    repl: &mut Repl<'_>,
    ev: &MouseEvent,
) -> Result<ReplControlFlow> {
    // Only handle left clicks for now
    if ev.kind == MouseEventKind::Down(MouseButton::Left) {
        // Get current cursor position to determine the active input line
        if let Ok((_, cursor_row)) = crossterm::cursor::position() {
            // Check if the click is on the same row as the input
            if ev.row == cursor_row {
                let prompt_width = repl.prompt_mark_width as u16;

                // Ensure the click is after the prompt mark
                if ev.column >= prompt_width {
                    let target_width = (ev.column - prompt_width) as usize;

                    // Update the cursor position in the input buffer
                    repl.input.set_cursor_from_display_width(target_width);

                    // Trigger a redraw of the input line to reflect the new cursor position
                    let mut renderer = TerminalRenderer::new();
                    repl.print_input(&mut renderer, false, false);
                    renderer.flush().ok();
                }
            }
        }
    }

    Ok(ReplControlFlow::Continue)
}
