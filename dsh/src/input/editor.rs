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

mod metrics;
mod render;
mod words;

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

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// Get the character at the given cursor index (not byte index)
    pub fn char_at(&self, idx: usize) -> Option<char> {
        if idx >= self.indices.len() {
            return None;
        }
        let byte_pos = self.indices[idx];
        self.input[byte_pos..].chars().next()
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
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.input.as_str())
    }
}

#[cfg(test)]
mod tests;
