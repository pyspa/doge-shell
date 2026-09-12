use super::config::{ColorType, InputConfig};
use crate::completion::shell_token::{self, SeparatorMode};
use crate::parser::{self, Rule};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use pest::Span;
use pest::iterators::Pairs;
use std::cmp::min;
use std::fmt;
use std::io::Write;
use unicode_width::UnicodeWidthChar;

const INITIAL_CAP: usize = 256;

/// Upper bound on retained undo snapshots for a single line.
const MAX_UNDO_DEPTH: usize = 100;

/// Buffer state captured for undo/redo.
#[derive(Debug, Clone)]
struct Snapshot {
    input: String,
    cursor: usize,
}

/// Kind of the last edit, used to coalesce a run of typed characters into a
/// single undo step instead of one step per keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Debug, Clone)]
pub struct Input {
    config: InputConfig,
    cursor: usize,
    input: String,
    indices: Vec<usize>,
    /// Cached display width of the full input string (updated on modification)
    cached_display_width: usize,

    /// Text removed by the most recent kill (Ctrl-K / Ctrl-U / Ctrl-W),
    /// re-insertable with Ctrl-Y.
    kill_ring: String,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    last_edit_kind: Option<EditKind>,
    /// Whether the previously typed character was whitespace, used to break the
    /// undo run at word boundaries.
    last_insert_was_space: bool,
    /// Set while a compound edit runs so nested primitives don't each record a
    /// snapshot (e.g. `delete_word_backward` calling `backspace` in a loop).
    suppress_undo: bool,

    pub completion: Option<String>,
    pub color_ranges: Option<Vec<(usize, usize, ColorType)>>, // (start, end, color_type)
    pub can_execute: bool,
}

impl Input {
    pub fn new(config: InputConfig) -> Input {
        Input {
            config,
            cursor: 0,
            input: String::with_capacity(INITIAL_CAP),
            indices: Vec::with_capacity(INITIAL_CAP),
            cached_display_width: 0,
            kill_ring: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit_kind: None,
            last_insert_was_space: false,
            suppress_undo: false,
            completion: None,
            color_ranges: None,
            can_execute: false,
        }
    }

    /// Record the pre-edit state so it can be restored by `undo`.
    ///
    /// Consecutive inserts coalesce into one step; any other edit starts a new
    /// one. Recording an edit invalidates the redo stack, as usual.
    fn record_undo(&mut self, kind: EditKind) {
        if self.suppress_undo {
            return;
        }
        if kind == EditKind::Insert && self.last_edit_kind == Some(EditKind::Insert) {
            self.redo_stack.clear();
            return;
        }

        self.undo_stack.push(Snapshot {
            input: self.input.clone(),
            cursor: self.cursor,
        });
        if self.undo_stack.len() > MAX_UNDO_DEPTH {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.last_edit_kind = Some(kind);
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.input = snapshot.input;
        self.update_indices();
        self.recalculate_display_width();
        self.cursor = min(snapshot.cursor, self.len());
        self.color_ranges = None;
        self.completion = None;
        self.last_edit_kind = None;
        self.last_insert_was_space = false;
    }

    /// Restore the previous buffer state. Returns false when there is nothing
    /// left to undo.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(Snapshot {
            input: self.input.clone(),
            cursor: self.cursor,
        });
        self.restore(previous);
        true
    }

    /// Re-apply the most recently undone state.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(Snapshot {
            input: self.input.clone(),
            cursor: self.cursor,
        });
        self.restore(next);
        true
    }

    /// Text held by the most recent kill.
    pub fn kill_ring(&self) -> &str {
        &self.kill_ring
    }

    /// Insert the kill ring at the cursor. Returns false when it is empty.
    pub fn yank(&mut self) -> bool {
        if self.kill_ring.is_empty() {
            return false;
        }
        let text = std::mem::take(&mut self.kill_ring);
        self.insert_str(&text);
        self.kill_ring = text;
        self.last_edit_kind = Some(EditKind::Other);
        true
    }

    pub fn reset(&mut self, input: String) {
        self.record_undo(EditKind::Other);
        self.input = input;
        self.update_indices();
        self.recalculate_display_width();
        self.move_to_end();
        self.color_ranges = None;
    }

    pub fn reset_with_color_ranges(
        &mut self,
        input: String,
        color_ranges: Vec<(usize, usize, ColorType)>,
    ) {
        self.record_undo(EditKind::Other);
        self.input = input;
        self.update_indices();
        self.recalculate_display_width();
        self.move_to_end();
        self.color_ranges = Some(color_ranges);
    }

    pub fn as_str(&self) -> &str {
        self.input.as_str()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.cursor = 0;
        self.input.clear();
        self.indices.clear();
        self.cached_display_width = 0;
        self.color_ranges = None;
        // A cleared buffer starts a new command line; its edit history is gone.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.last_insert_was_space = false;
    }

    pub fn move_to_begin(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor = self.len();
    }

    pub fn insert(&mut self, ch: char) {
        // Break the undo run at word boundaries. Without this, a whole typed
        // line coalesces into one step and a single undo wipes everything.
        if ch.is_whitespace() != self.last_insert_was_space {
            self.last_edit_kind = None;
        }
        self.record_undo(EditKind::Insert);
        self.last_insert_was_space = ch.is_whitespace();

        let byte_index = self.byte_index();
        self.input.insert(byte_index, ch);

        let char_len = ch.len_utf8();
        let insert_pos = self.cursor;
        self.indices.insert(insert_pos, byte_index);
        self.shift_indices_from(insert_pos + 1, char_len as isize);
        self.cursor += 1;

        // Incrementally update display width
        self.cached_display_width += ch.width().unwrap_or_default();
        self.color_ranges = None;
    }

    pub fn insert_str(&mut self, string: &str) {
        if string.is_empty() {
            return;
        }
        self.record_undo(EditKind::Other);

        let byte_index = self.byte_index();
        self.input.insert_str(byte_index, string);

        let inserted_chars = string.chars().count();
        let mut offsets = Vec::with_capacity(inserted_chars);
        for (rel, _) in string.char_indices() {
            offsets.push(byte_index + rel);
        }
        let advance = string.len();

        let insert_pos = self.cursor;
        self.indices.splice(insert_pos..insert_pos, offsets);
        self.shift_indices_from(insert_pos + inserted_chars, advance as isize);
        self.cursor += inserted_chars;

        // Incrementally update display width
        let added_width: usize = string.chars().map(|c| c.width().unwrap_or_default()).sum();
        self.cached_display_width += added_width;
        self.color_ranges = None;
    }

    pub fn replace_range_chars(&mut self, start: usize, end: usize, replacement: &str) -> bool {
        if start > end || end > self.len() {
            return false;
        }

        let start_byte = if start == self.len() {
            self.input.len()
        } else {
            self.indices[start]
        };
        let end_byte = if end == self.len() {
            self.input.len()
        } else {
            self.indices[end]
        };

        self.input.replace_range(start_byte..end_byte, replacement);
        self.update_indices();
        self.recalculate_display_width();
        self.cursor = start + replacement.chars().count();
        self.color_ranges = None;
        true
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 && self.cursor <= self.indices.len() {
            self.record_undo(EditKind::Delete);
            let remove_index = self.cursor - 1;
            let byte_index = self.indices[remove_index];
            let char_len = self.char_len_at(remove_index);

            // Get the character being removed for display width calculation
            let removed_char = self.input[byte_index..].chars().next();
            if let Some(ch) = removed_char {
                self.cached_display_width = self
                    .cached_display_width
                    .saturating_sub(ch.width().unwrap_or_default());
            }

            self.input.drain(byte_index..byte_index + char_len);
            self.indices.remove(remove_index);
            self.shift_indices_from(remove_index, -(char_len as isize));
            self.cursor -= 1;
            self.color_ranges = None;
        }
    }

    pub fn backspacen(&mut self, n: usize) {
        for _ in 0..n {
            self.backspace();
        }
    }

    /// Moves the cursor to an absolute char offset, clamped to the buffer.
    pub fn move_to(&mut self, position: usize) {
        self.cursor = min(self.len(), position);
    }

    pub fn move_by(&mut self, offset: isize) {
        if offset < 0 {
            self.cursor = self.cursor.saturating_sub(offset.unsigned_abs());
        } else {
            self.cursor = min(self.len(), self.cursor + offset.unsigned_abs());
        }
    }

    pub fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let chars: Vec<char> = self.input.chars().collect();
        let mut idx = self.cursor;

        // Skip trailing spaces
        while idx > 0 && chars[idx - 1].is_whitespace() {
            idx -= 1;
        }

        // Find start of word
        while idx > 0 && !chars[idx - 1].is_whitespace() {
            idx -= 1;
        }

        self.kill_ring = chars[idx..self.cursor].iter().collect();

        // One undo step for the whole word, not one per character.
        self.record_undo(EditKind::Other);
        let word_len = self.cursor - idx;
        self.suppress_undo = true;
        for _ in 0..word_len {
            self.backspace();
        }
        self.suppress_undo = false;
    }

    pub fn delete_to_end(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        self.record_undo(EditKind::Other);
        let byte_index = self.byte_index();

        self.kill_ring = self.input[byte_index..].to_string();

        // Remove content from string
        self.input.truncate(byte_index);

        // Remove indices
        self.indices.truncate(self.cursor);

        // Recalculate display width (simpler than tracking removed chars)
        self.recalculate_display_width();
        self.color_ranges = None;

        // Cursor position remains effectively the same (now at end)
    }

    pub fn delete_to_beginning(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.record_undo(EditKind::Other);
        let byte_index = self.byte_index();

        self.kill_ring = self.input[..byte_index].to_string();

        // Remove content from string
        self.input.drain(0..byte_index);

        // Remove indices
        self.indices.drain(0..self.cursor);

        // Shift remaining indices
        let shift_amount = -(byte_index as isize);
        let delta = shift_amount;
        // In this specific case, since we drained from 0, the remaining indices need to be shifted down.
        // But indices are simply byte offsets.
        // Example: "abc", cursor at 1 ('b'). indices=[0, 1, 2].
        // byte_index=1. drain 0..1 removes 'a'. input="bc".
        // indices drain 0..1 removes 0. indices=[1, 2].
        // We want indices=[0, 1]. So subtract 1 from everything.

        let delta_abs = delta.unsigned_abs();
        for idx in &mut self.indices {
            *idx -= delta_abs;
        }

        self.cursor = 0;

        // Recalculate display width
        self.recalculate_display_width();
        self.color_ranges = None;
    }

    pub fn move_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let chars: Vec<char> = self.input.chars().collect();
        let mut idx = self.cursor;

        // Skip spaces to the left
        while idx > 0 && chars[idx - 1].is_whitespace() {
            idx -= 1;
        }

        // Skip non-spaces
        while idx > 0 && !chars[idx - 1].is_whitespace() {
            idx -= 1;
        }

        self.cursor = idx;
    }

    pub fn move_word_right(&mut self) {
        if self.cursor >= self.len() {
            return;
        }

        let chars: Vec<char> = self.input.chars().collect();
        let mut idx = self.cursor;
        let len = chars.len();

        // Skip non-spaces to the right (current word)
        while idx < len && !chars[idx].is_whitespace() {
            idx += 1;
        }

        // Skip spaces to next word
        while idx < len && chars[idx].is_whitespace() {
            idx += 1;
        }

        self.cursor = idx;
    }

    fn byte_index(&self) -> usize {
        if self.cursor == self.indices.len() {
            self.input.len()
        } else {
            self.indices[self.cursor]
        }
    }

    fn shift_indices_from(&mut self, start: usize, delta: isize) {
        if delta == 0 || start >= self.indices.len() {
            return;
        }

        if delta.is_positive() {
            let delta = delta as usize;
            for idx in &mut self.indices[start..] {
                *idx += delta;
            }
        } else {
            let delta = (-delta) as usize;
            for idx in &mut self.indices[start..] {
                *idx -= delta;
            }
        }
    }

    fn char_len_at(&self, index: usize) -> usize {
        if index >= self.indices.len() {
            return 0;
        }

        if index + 1 < self.indices.len() {
            self.indices[index + 1] - self.indices[index]
        } else {
            self.input.len().saturating_sub(self.indices[index])
        }
    }

    fn update_indices(&mut self) {
        self.indices.clear();
        for index in self.input.char_indices() {
            self.indices.push(index.0);
        }
    }

    pub fn len(&self) -> usize {
        self.indices.len()
    }

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

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub fn get_cursor_word(&self) -> Result<Option<(Rule, Span<'_>)>> {
        parser::get_pos_word(self.input.as_str(), self.cursor)
    }

    /// Get the character at the given cursor index (not byte index)
    pub fn char_at(&self, idx: usize) -> Option<char> {
        if idx >= self.indices.len() {
            return None;
        }
        let byte_pos = self.indices[idx];
        self.input[byte_pos..].chars().next()
    }

    /// Fallback computation for completion word when parser cannot identify it (e.g. after redirects)
    pub fn get_completion_word_fallback(&self) -> Option<String> {
        if self.input.is_empty() || self.cursor == 0 {
            return None;
        }

        let token = shell_token::token_at_char_cursor(
            self.input.as_str(),
            self.cursor,
            SeparatorMode::CompletionRange,
        )?;
        if self.cursor <= token.char_start || token.raw.is_empty() {
            return None;
        }

        Some(token.raw)
    }

    /// Get the current word at cursor position for abbreviation expansion
    /// Returns the word that could be an abbreviation
    pub fn get_current_word_for_abbr(&self) -> Option<String> {
        if self.input.is_empty() || self.cursor == 0 {
            tracing::debug!(
                "ABBR_WORD: Input empty or cursor at 0, input='{}', cursor={}",
                self.input,
                self.cursor
            );
            return None;
        }

        // Find word boundaries - look backwards from cursor
        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.cursor;

        tracing::debug!(
            "ABBR_WORD: Starting word detection, input='{}', cursor={}, chars.len()={}",
            self.input,
            self.cursor,
            chars.len()
        );

        // Move start backwards to find beginning of word
        while start > 0 {
            let ch = chars[start - 1];
            if ch.is_whitespace() || ch == '|' || ch == '&' || ch == ';' || ch == '(' || ch == ')' {
                break;
            }
            start -= 1;
        }

        // Extract the word from start to cursor
        if start < self.cursor {
            let word: String = chars[start..self.cursor].iter().collect();
            tracing::debug!(
                "ABBR_WORD: Extracted word='{}' from range {}..{}",
                word,
                start,
                self.cursor
            );
            if !word.trim().is_empty() {
                Some(word)
            } else {
                tracing::debug!("ABBR_WORD: Word is empty after trim");
                None
            }
        } else {
            tracing::debug!("ABBR_WORD: start >= cursor, no word found");
            None
        }
    }

    /// Replace the current word with an expansion
    /// Used for abbreviation expansion
    pub fn replace_current_word(&mut self, expansion: &str) -> bool {
        if let Some(word) = self.get_current_word_for_abbr() {
            let word_len = word.chars().count();

            // Move cursor back to start of word
            if self.cursor >= word_len {
                self.cursor -= word_len;

                // Remove the word by deleting characters at current position
                for _ in 0..word_len {
                    if self.cursor < self.len() {
                        self.delete_char();
                    }
                }

                // Insert the expansion
                for ch in expansion.chars() {
                    self.insert(ch);
                }

                true
            } else {
                false
            }
        } else {
            false
        }
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.len() || self.cursor >= self.indices.len() {
            return;
        }
        self.record_undo(EditKind::Delete);

        let byte_index = self.indices[self.cursor];
        let char_len = self.char_len_at(self.cursor);

        // Get the character being removed for display width calculation
        let removed_char = self.input[byte_index..].chars().next();
        if let Some(ch) = removed_char {
            self.cached_display_width = self
                .cached_display_width
                .saturating_sub(ch.width().unwrap_or_default());
        }

        self.input.drain(byte_index..byte_index + char_len);
        self.indices.remove(self.cursor);
        self.shift_indices_from(self.cursor, -(char_len as isize));
        self.color_ranges = None;
    }

    pub fn get_words(&self) -> Result<Vec<(Rule, Span<'_>, bool)>> {
        parser::get_words(self.input.as_str(), self.cursor)
    }

    pub fn get_words_from_pairs<'a>(&self, pairs: Pairs<'a, Rule>) -> Vec<(Rule, Span<'a>, bool)> {
        parser::get_words_from_pairs(pairs, self.cursor)
    }

    /// Write the input line (and any ghost suffix) into `out`.
    ///
    /// `out` is the caller's frame buffer, not the terminal: it must not be
    /// flushed here. The redraw writes the clear sequence, the prompt mark, this
    /// line, the hint and the cursor restore into one buffer so the terminal sees
    /// a single atomic frame; flushing mid-way would split it across writes.
    pub fn print<W: Write>(&self, out: &mut W, ghost_suffix: Option<&str>) {
        if let Some(color_ranges) = &self.color_ranges {
            // Write colored segments directly to reduce allocation
            self.write_colored_ranges_to(out, color_ranges).ok();
        } else {
            for (i, line) in self.as_str().split('\n').enumerate() {
                if i > 0 {
                    out.write_all(b"\r\n").ok();
                }
                out.write_fmt(format_args!("{}", line.with(self.config.fg_color)))
                    .ok();
            }
        }

        if let Some(suffix) = ghost_suffix.filter(|s| !s.is_empty()) {
            for (i, line) in suffix.split('\n').enumerate() {
                if i > 0 {
                    out.write_all(b"\r\n").ok();
                }
                out.write_fmt(format_args!("{}", line.with(self.config.ghost_color)))
                    .ok();
            }
        }
    }

    /// Write colored string from color ranges directly to writer
    /// Note: color_ranges must be sorted by start position (ensured by compute_color_ranges)
    fn write_colored_ranges_to<W: Write>(
        &self,
        writer: &mut W,
        color_ranges: &[(usize, usize, ColorType)],
    ) -> std::io::Result<()> {
        use crossterm::style::Stylize;

        let input_str = self.as_str();
        let mut last_end = 0;

        // color_ranges is already sorted by start position in compute_color_ranges
        for &(start, end, color_type) in color_ranges {
            // Add any uncolored text before this range
            if start > last_end {
                let prefix = &input_str[last_end..start];
                for (i, line) in prefix.split('\n').enumerate() {
                    if i > 0 {
                        write!(writer, "\r\n")?;
                    }
                    write!(writer, "{}", line.with(self.config.fg_color))?;
                }
            }

            // Add the colored text for this range
            let colored_text = &input_str[start..end];
            let color = match color_type {
                ColorType::CommandExists => self.config.command_exists_color,
                ColorType::CommandNotExists => self.config.command_not_exists_color,
                ColorType::Argument => self.config.argument_color,
                ColorType::Variable => self.config.variable_color,
                ColorType::Assignment => self.config.assignment_color,
                ColorType::SingleQuote => self.config.single_quote_color,
                ColorType::DoubleQuote => self.config.double_quote_color,
                ColorType::Redirect => self.config.redirect_color,
                ColorType::Operator => self.config.operator_color,
                ColorType::Pipe => self.config.pipe_color,
                ColorType::Background => self.config.background_color,
                ColorType::ProcSubst => self.config.proc_subst_color,
                ColorType::Error => self.config.error_color,
                ColorType::ValidPath => self.config.valid_path_color,
                ColorType::HistoryMatch => self.config.history_match_fg_color,
            };

            for (i, line) in colored_text.split('\n').enumerate() {
                if i > 0 {
                    write!(writer, "\r\n")?;
                }
                if matches!(color_type, ColorType::HistoryMatch) {
                    write!(
                        writer,
                        "{}",
                        line.with(color).on(self.config.history_match_bg_color)
                    )?;
                } else {
                    write!(writer, "{}", line.with(color))?;
                }
            }

            // Update the last processed position
            last_end = end.max(last_end);
        }

        // Add any remaining uncolored text after the last range
        if last_end < input_str.len() {
            let suffix = &input_str[last_end..];
            for (i, line) in suffix.split('\n').enumerate() {
                if i > 0 {
                    write!(writer, "\r\n")?;
                }
                write!(writer, "{}", line.with(self.config.fg_color))?;
            }
        }

        Ok(())
    }

    pub fn fg_color(&self) -> Color {
        self.config.fg_color
    }

    pub fn command_exists_color(&self) -> Color {
        self.config.command_exists_color
    }

    pub fn command_not_exists_color(&self) -> Color {
        self.config.command_not_exists_color
    }

    pub fn argument_color(&self) -> Color {
        self.config.argument_color
    }

    pub fn completion_color(&self) -> Color {
        self.config.completion_color
    }

    pub fn ghost_color(&self) -> Color {
        self.config.ghost_color
    }

    /// Append the inline completion candidate to the caller's frame buffer.
    /// Like [`Input::print`], this must not flush — see that method's note.
    pub fn print_candidates<W: Write>(&mut self, out: &mut W, completion: String) {
        let current_byte = self.byte_index();
        let is_end = current_byte == self.input.len();

        out.write_fmt(format_args!(
            "{}",
            completion.with(self.config.completion_color)
        ))
        .ok();

        if !is_end {
            let tmp = &self.input[current_byte..];
            out.write_fmt(format_args!("{}", tmp.with(self.config.fg_color)))
                .ok();
        }
    }

    pub fn split_current_pos(&self) -> Option<(&str, &str)> {
        let current_byte = self.byte_index();
        if current_byte == self.input.len() {
            None
        } else {
            let pre = &self.input[..current_byte];
            let post = &self.input[current_byte..];
            Some((pre, post))
        }
    }
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.input.as_str())
    }
}

#[cfg(test)]
mod tests;
