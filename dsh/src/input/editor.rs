use super::config::{ColorType, InputConfig};
use crate::completion::shell_token::{self, SeparatorMode};
use crate::parser::{self, Rule};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use pest::Span;
use pest::iterators::Pairs;
use std::cmp::min;
use std::fmt;
use std::io::{BufWriter, Write};
use unicode_width::UnicodeWidthChar;

const INITIAL_CAP: usize = 256;

#[derive(Debug, Clone)]
pub struct Input {
    config: InputConfig,
    cursor: usize,
    input: String,
    indices: Vec<usize>,
    /// Cached display width of the full input string (updated on modification)
    cached_display_width: usize,

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
            completion: None,
            color_ranges: None,
            can_execute: false,
        }
    }

    pub fn reset(&mut self, input: String) {
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
    }

    pub fn move_to_begin(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor = self.len();
    }

    pub fn insert(&mut self, ch: char) {
        let byte_index = self.byte_index();
        self.input.insert(byte_index, ch);

        let char_len = ch.len_utf8();
        let insert_pos = self.cursor;
        self.indices.insert(insert_pos, byte_index);
        self.shift_indices_from(insert_pos + 1, char_len as isize);
        self.cursor += 1;

        // Incrementally update display width
        self.cached_display_width += ch.width().unwrap_or_default();
    }

    pub fn insert_str(&mut self, string: &str) {
        if string.is_empty() {
            return;
        }

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
        }
    }

    pub fn backspacen(&mut self, n: usize) {
        for _ in 0..n {
            self.backspace();
        }
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

        let word_len = self.cursor - idx;
        for _ in 0..word_len {
            self.backspace();
        }
    }

    pub fn delete_to_end(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let byte_index = self.byte_index();

        // Remove content from string
        self.input.truncate(byte_index);

        // Remove indices
        self.indices.truncate(self.cursor);

        // Recalculate display width (simpler than tracking removed chars)
        self.recalculate_display_width();

        // Cursor position remains effectively the same (now at end)
    }

    pub fn delete_to_beginning(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte_index = self.byte_index();

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
    }

    pub fn get_words(&self) -> Result<Vec<(Rule, Span<'_>, bool)>> {
        parser::get_words(self.input.as_str(), self.cursor)
    }

    pub fn get_words_from_pairs<'a>(&self, pairs: Pairs<'a, Rule>) -> Vec<(Rule, Span<'a>, bool)> {
        parser::get_words_from_pairs(pairs, self.cursor)
    }

    pub fn print<W: Write>(&self, out: &mut W, ghost_suffix: Option<&str>) {
        let mut writer = BufWriter::new(out);

        if let Some(color_ranges) = &self.color_ranges {
            // Write colored segments directly to reduce allocation
            self.write_colored_ranges_to(&mut writer, color_ranges).ok();
        } else {
            for (i, line) in self.as_str().split('\n').enumerate() {
                if i > 0 {
                    writer.write_all(b"\r\n").ok();
                }
                writer
                    .write_fmt(format_args!("{}", line.with(self.config.fg_color)))
                    .ok();
            }
        }

        if let Some(suffix) = ghost_suffix.filter(|s| !s.is_empty()) {
            for (i, line) in suffix.split('\n').enumerate() {
                if i > 0 {
                    writer.write_all(b"\r\n").ok();
                }
                writer
                    .write_fmt(format_args!("{}", line.with(self.config.ghost_color)))
                    .ok();
            }
        }

        // Ensure all buffered output is written immediately
        writer.flush().ok();
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
                ColorType::SingleQuote => self.config.single_quote_color,
                ColorType::DoubleQuote => self.config.double_quote_color,
                ColorType::Redirect => self.config.redirect_color,
                ColorType::Operator => self.config.operator_color,
                ColorType::Pipe => self.config.pipe_color,
                ColorType::Background => self.config.background_color,
                ColorType::ProcSubst => self.config.proc_subst_color,
                ColorType::Error => self.config.error_color,
                ColorType::ValidPath => self.config.valid_path_color,
            };

            for (i, line) in colored_text.split('\n').enumerate() {
                if i > 0 {
                    write!(writer, "\r\n")?;
                }
                write!(writer, "{}", line.with(color))?;
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

    #[allow(dead_code)]
    pub fn fg_color(&self) -> Color {
        self.config.fg_color
    }

    #[allow(dead_code)]
    pub fn command_exists_color(&self) -> Color {
        self.config.command_exists_color
    }

    #[allow(dead_code)]
    pub fn command_not_exists_color(&self) -> Color {
        self.config.command_not_exists_color
    }

    #[allow(dead_code)]
    pub fn argument_color(&self) -> Color {
        self.config.argument_color
    }

    #[allow(dead_code)]
    pub fn completion_color(&self) -> Color {
        self.config.completion_color
    }

    #[allow(dead_code)]
    pub fn ghost_color(&self) -> Color {
        self.config.ghost_color
    }

    pub fn print_candidates<W: Write>(&mut self, out: &mut W, completion: String) {
        let mut writer = BufWriter::new(out);
        let current_byte = self.byte_index();
        let is_end = current_byte == self.input.len();

        // Write colored completion directly to the writer
        writer
            .write_fmt(format_args!(
                "{}",
                completion.with(self.config.completion_color)
            ))
            .ok();

        if !is_end {
            let tmp = &self.input[current_byte..];
            writer
                .write_fmt(format_args!("{}", tmp.with(self.config.fg_color)))
                .ok();
        }

        // Ensure all buffered output is written immediately
        writer.flush().ok();
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
mod tests {
    use super::*;

    #[test]
    fn test_input_creation_and_display() {
        let config = InputConfig::default();
        let input = Input::new(config);

        assert_eq!(input.as_str(), "");
        assert_eq!(input.cursor(), 0);
        assert_eq!(format!("{input}"), "");
    }

    #[test]
    fn test_input_operations() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Character input test
        input.insert('h');
        input.insert('i');
        assert_eq!(input.as_str(), "hi");
        assert_eq!(input.cursor(), 2);

        // Cursor movement test
        input.move_to_end();
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_replace_range_chars_ascii() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.insert_str("git status");

        assert!(input.replace_range_chars(4, 10, "stash"));
        assert_eq!(input.as_str(), "git stash");
        assert_eq!(input.cursor(), 9);
    }

    #[test]
    fn test_replace_range_chars_multibyte() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.insert_str("cmd あい うえ");

        assert!(input.replace_range_chars(4, 6, "お"));
        assert_eq!(input.as_str(), "cmd お うえ");
        assert_eq!(input.cursor(), 5);
        assert_eq!(input.cursor_pos(80, 0).0, 6);
    }

    #[test]
    fn test_replace_range_chars_rejects_invalid_range() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.insert_str("abc");

        assert!(!input.replace_range_chars(3, 1, "x"));
        assert!(!input.replace_range_chars(0, 4, "x"));
        assert_eq!(input.as_str(), "abc");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn test_unicode_width_calculation() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Test ASCII characters (width = 1 each)
        input.insert('a');
        input.insert('b');
        assert_eq!(input.cursor(), 2);
        assert_eq!(input.cursor_pos(80, 0).0, 2);

        // Clear and test Japanese characters (width = 2 each)
        input.clear();
        input.insert('あ'); // Japanese hiragana 'a'
        input.insert('い'); // Japanese hiragana 'i'
        assert_eq!(input.cursor(), 2); // 2 characters
        assert_eq!(input.cursor_pos(80, 0).0, 4); // 4 display width

        // Test mixed ASCII and Japanese
        input.clear();
        input.insert('a'); // width = 1
        input.insert('あ'); // width = 2
        input.insert('b'); // width = 1
        assert_eq!(input.cursor(), 3); // 3 characters
        assert_eq!(input.cursor_pos(80, 0).0, 4); // 1 + 2 + 1 = 4 display width
    }

    #[test]
    fn test_cursor_movement_with_unicode() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Insert mixed characters
        input.insert('a'); // width = 1
        input.insert('あ'); // width = 2
        input.insert('b'); // width = 1

        // Move cursor to different positions and check display width
        input.move_to_begin();
        assert_eq!(input.cursor(), 0);
        assert_eq!(input.cursor_pos(80, 0).0, 0);

        input.move_by(1); // After 'a'
        assert_eq!(input.cursor(), 1);
        assert_eq!(input.cursor_pos(80, 0).0, 1);

        input.move_by(1); // After 'あ'
        assert_eq!(input.cursor(), 2);
        assert_eq!(input.cursor_pos(80, 0).0, 3); // 1 + 2

        input.move_by(1); // After 'b'
        assert_eq!(input.cursor(), 3);
        assert_eq!(input.cursor_pos(80, 0).0, 4); // 1 + 2 + 1
    }

    #[test]
    fn test_backspace_with_multibyte_characters() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        input.insert('a');
        input.insert('あ');
        input.insert('b');

        input.backspace();
        assert_eq!(input.as_str(), "aあ");
        assert_eq!(input.cursor(), 2);

        input.backspace();
        assert_eq!(input.as_str(), "a");
        assert_eq!(input.cursor(), 1);

        input.backspace();
        assert_eq!(input.as_str(), "");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_delete_char_with_multibyte_characters() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        input.insert_str("ab😀c");
        input.move_to_end();
        input.move_by(-2);

        input.delete_char();
        assert_eq!(input.as_str(), "abc");
        assert_eq!(input.cursor(), 2);
    }

    // Moved display_width tests to utils.rs

    #[test]
    fn test_delete_to_end() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Test at beginning
        input.reset("hello".to_string());
        input.move_to_begin();
        input.delete_to_end();
        assert_eq!(input.as_str(), "");
        assert_eq!(input.cursor(), 0);

        // Test in middle
        input.reset("hello".to_string());
        input.move_to_begin();
        input.move_by(2); // "he|llo"
        input.delete_to_end();
        assert_eq!(input.as_str(), "he");
        assert_eq!(input.cursor(), 2);

        // Test at end
        input.reset("hello".to_string());
        input.move_to_end();
        input.delete_to_end();
        assert_eq!(input.as_str(), "hello");
        assert_eq!(input.cursor(), 5);

        // Test with multi-byte
        input.reset("あいうえお".to_string());
        input.move_to_begin();
        input.move_by(2); // "あい|うえお"
        input.delete_to_end();
        assert_eq!(input.as_str(), "あい");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_delete_to_beginning() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Test at beginning (nothing happens)
        input.reset("hello".to_string());
        input.move_to_begin();
        input.delete_to_beginning();
        assert_eq!(input.as_str(), "hello");
        assert_eq!(input.cursor(), 0);

        // Test in middle
        input.reset("hello".to_string());
        input.move_to_begin();
        input.move_by(2); // "he|llo"
        input.delete_to_beginning();
        assert_eq!(input.as_str(), "llo");
        assert_eq!(input.cursor(), 0);

        // Test at end
        input.reset("hello".to_string());
        input.move_to_end();
        input.delete_to_beginning();
        assert_eq!(input.as_str(), "");
        assert_eq!(input.cursor(), 0);

        // Test with multi-byte
        input.reset("あいうえお".to_string());
        input.move_to_begin();
        input.move_by(2); // "あい|うえお"
        input.delete_to_beginning();
        assert_eq!(input.as_str(), "うえお");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_completion_word_fallback_for_redirect() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        for ch in "cat > fo".chars() {
            input.insert(ch);
        }

        let fallback = input.get_completion_word_fallback();
        assert_eq!(fallback.as_deref(), Some("fo"));
    }

    #[test]
    fn test_completion_word_fallback_handles_whitespace_boundary() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        for ch in "echo foo".chars() {
            input.insert(ch);
        }

        let fallback = input.get_completion_word_fallback();
        assert_eq!(fallback.as_deref(), Some("foo"));

        input.insert(' ');
        let fallback_after_space = input.get_completion_word_fallback();
        assert_eq!(fallback_after_space, None);
    }

    #[test]
    fn test_completion_word_fallback_preserves_escaped_space_token() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.reset(r#"cat dir\ with\ space/fo"#.to_string());

        let fallback = input.get_completion_word_fallback();

        assert_eq!(fallback.as_deref(), Some(r#"dir\ with\ space/fo"#));
    }

    #[test]
    fn test_completion_word_fallback_preserves_quoted_tokens() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.reset(r#"cat "dir with space/fo"#.to_string());

        let fallback = input.get_completion_word_fallback();

        assert_eq!(fallback.as_deref(), Some(r#""dir with space/fo"#));

        input.reset(r#"cat 'dir with space/fo"#.to_string());
        let single_quoted = input.get_completion_word_fallback();

        assert_eq!(single_quoted.as_deref(), Some(r#"'dir with space/fo"#));
    }

    #[test]
    fn test_completion_word_fallback_respects_operator_separator() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.reset("cmd | foo".to_string());

        let fallback = input.get_completion_word_fallback();

        assert_eq!(fallback.as_deref(), Some("foo"));
    }

    #[test]
    fn test_completion_word_fallback_returns_none_on_token_start() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.reset("cmd | foo".to_string());
        input.move_to_begin();
        input.move_by(6);

        let fallback = input.get_completion_word_fallback();

        assert_eq!(fallback, None);
    }

    #[test]
    fn test_word_navigation_and_deletion() {
        let config = InputConfig::default();
        let mut input = Input::new(config);
        input.insert_str("echo hello world");

        // Initial state: "echo hello world|"
        assert_eq!(input.cursor(), 16);

        // Test Ctrl+W (delete "world")
        input.delete_word_backward();
        assert_eq!(input.as_str(), "echo hello ");
        assert_eq!(input.cursor(), 11);

        // Test delete "hello"
        input.delete_word_backward();
        assert_eq!(input.as_str(), "echo ");
        assert_eq!(input.cursor(), 5);

        // Test delete "echo"
        input.delete_word_backward();
        assert_eq!(input.as_str(), "");
        assert_eq!(input.cursor(), 0);

        // restore
        input.insert_str("one two three");
        // "one two three|"

        // Test Move Left
        input.move_word_left(); // to start of "three"
        assert_eq!(input.cursor(), 8); // "one two |three"

        input.move_word_left(); // to start of "two"
        assert_eq!(input.cursor(), 4); // "one |two three"

        input.move_word_left(); // to start of "one"
        assert_eq!(input.cursor(), 0); // "|one two three"

        // Test Move Right
        input.move_word_right(); // to end of "one"
        assert_eq!(input.cursor(), 4); // "one| two three"

        input.move_word_right();
        assert_eq!(input.cursor(), 8); // start of "three"

        input.move_word_right();
        assert_eq!(input.cursor(), 13); // end of string
    }

    #[test]
    fn test_set_cursor_from_display_width() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Test with ASCII
        input.insert_str("abcdef");

        // Target width exact matches
        input.set_cursor_from_display_width(0);
        assert_eq!(input.cursor(), 0);

        input.set_cursor_from_display_width(1);
        assert_eq!(input.cursor(), 1);

        input.set_cursor_from_display_width(3);
        assert_eq!(input.cursor(), 3);

        // Target width beyond end of string
        input.set_cursor_from_display_width(100);
        assert_eq!(input.cursor(), 6);

        // Test with multi-byte characters
        input.clear();
        input.insert_str("あいう");

        // The 'あ' is width 2.
        // Clicking at width 0 snaps to 0.
        input.set_cursor_from_display_width(0);
        assert_eq!(input.cursor(), 0);

        // Clicking at width 1 (middle of 'あ') snaps to 1 (after 'あ').
        input.set_cursor_from_display_width(1);
        assert_eq!(input.cursor(), 1);

        // Clicking at width 2 (start of 'い') snaps to 1.
        input.set_cursor_from_display_width(2);
        assert_eq!(input.cursor(), 1);

        // Clicking at width 3 (middle of 'い') snaps to 2.
        input.set_cursor_from_display_width(3);
        assert_eq!(input.cursor(), 2);

        // Test mixed characters
        input.clear();
        input.insert_str("aあbいc");

        // "a" (0..1) -> width 0-1
        // "あ" (1..2) -> width 1-3
        // "b" (2..3) -> width 3-4
        // "い" (3..4) -> width 4-6
        // "c" (4..5) -> width 6-7

        input.set_cursor_from_display_width(0);
        assert_eq!(input.cursor(), 0);

        input.set_cursor_from_display_width(1);
        assert_eq!(input.cursor(), 1);

        input.set_cursor_from_display_width(2);
        assert_eq!(input.cursor(), 2);

        input.set_cursor_from_display_width(3);
        assert_eq!(input.cursor(), 2);

        input.set_cursor_from_display_width(4);
        assert_eq!(input.cursor(), 3);

        input.set_cursor_from_display_width(10);
        assert_eq!(input.cursor(), 5);
    }
}
