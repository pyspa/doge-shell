//! Terminal geometry for the input line: where the cursor lands once the text is
//! wrapped into `columns`-wide rows (`cursor_pos`/`line_count`), the cached total
//! display width kept in step with every edit, and the inverse mapping a mouse
//! click needs (`set_cursor_from_display_width`).
use super::*;

impl Input {
    /// Returns the (display_column, visual_line_index) of the cursor.
    pub fn cursor_pos(&self, columns: usize, prompt_width: usize) -> (usize, usize) {
        if self.cursor == 0 {
            return (prompt_width, 0);
        }

        let cursor_byte_pos = self.byte_index();
        let text_to_cursor = &self.input[..cursor_byte_pos];

        let mut width = prompt_width;
        let mut lines = 0;

        for c in text_to_cursor.chars() {
            if c == '\n' {
                width = 0;
                lines += 1;
            } else {
                let w = c.width().unwrap_or_default();
                if columns > 0 && width + w > columns {
                    width = w;
                    lines += 1;
                } else {
                    width += w;
                }
            }
        }

        (width, lines)
    }

    /// Returns the total number of visual lines in the input.
    pub fn line_count(&self, columns: usize, prompt_width: usize) -> usize {
        if self.input.is_empty() {
            return 1;
        }

        let mut width = prompt_width;
        let mut lines = 1;

        for c in self.input.chars() {
            if c == '\n' {
                width = 0;
                lines += 1;
            } else {
                let w = c.width().unwrap_or_default();
                if columns > 0 && width + w > columns {
                    width = w;
                    lines += 1;
                } else {
                    width += w;
                }
            }
        }

        lines
    }

    /// Get the cached display width of the full input string
    pub fn display_width(&self) -> usize {
        self.cached_display_width
    }

    /// Recalculate and cache the display width of the full input string
    /// Call this after any modification to the input
    pub fn recalculate_display_width(&mut self) {
        self.cached_display_width = self
            .input
            .chars()
            .map(|c| c.width().unwrap_or_default())
            .sum();
    }

    /// Set the cursor position based on a target visual display width offset.
    /// This is used for mapping mouse clicks to string character positions.
    pub fn set_cursor_from_display_width(&mut self, target_width: usize) {
        let mut current_width = 0;
        let mut new_cursor = 0;

        for ch in self.input.chars() {
            let char_width = ch.width().unwrap_or_default();

            // Calculate distance to left and right edges.
            // Tie goes to the right edge (advancing the cursor).
            if 2 * target_width < 2 * current_width + char_width {
                break;
            }

            current_width += char_width;
            new_cursor += 1;
        }

        self.cursor = new_cursor;
    }
}
