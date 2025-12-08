use crate::parser::{self, Rule};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use pest::Span;
use pest::iterators::Pairs;
use std::cmp::min;
use std::fmt;
use std::io::{BufWriter, Write};
use unicode_width::UnicodeWidthChar;

/// Calculate the actual display width of a string, accounting for ANSI codes and Unicode width
pub fn display_width(input: &str) -> usize {
    let mut width = 0;
    let mut in_ansi_sequence = false;
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        if in_ansi_sequence {
            // End of ANSI sequence is usually a letter
            if ch.is_ascii_alphabetic() {
                in_ansi_sequence = false;
            }
            continue;
        }

        if ch == '\x1b' {
            // Check next char to confirm CSI sequence
            // We peak by cloning iterator or just consuming one more
            // Since we are in a loop, we can just check the next char.
            // But we need to be careful not to consume it if it's not part of sequence (unlikely for \x1b)
            // Ideally we check if next is '['

            // For simple ANSI stripping: \x1b followed by [ ... letter
            // We just assume it's an escape sequence start.
            // Let's peek the next char if possible, or just consume it.
            // Since we don't have peekable here without allocation/wrapping,
            // we'll use a slightly different approach or just consume.
            // However, `chars` is an iterator.
            // Let's try to be robust.

            // We can consume the next char.
            if let Some(next) = chars.next() {
                if next == '[' {
                    in_ansi_sequence = true;
                } else {
                    // It was just an ESC char? Treat as 0 width non-printable or 1?
                    // Usually ESC is non-printable.
                }
            }
        } else {
            width += unicode_width::UnicodeWidthChar::width_cjk(ch).unwrap_or(0);
        }
    }
    width
}

#[cfg(test)]
mod display_width_tests {
    use super::*;

    #[test]
    fn test_display_width_special_chars() {
        // "✘" is reported as 1 by unicode-width even in CJK mode, but often renders as 2.
        // The safety margin in prompt.rs handles this discrepancy.
        // We verify that width_cjk is active by checking a standard CJK character.
        assert_eq!(display_width("あ"), 2, "CJK character should be width 2");
        assert_eq!(display_width("✘"), 1, "Library reports 1 for ✘");
    }
}

const INITIAL_CAP: usize = 256;

#[derive(Debug, Clone)]
pub struct InputConfig {
    pub fg_color: Color,                 // Normal input text (white)
    pub command_exists_color: Color,     // Command that exists (blue)
    pub command_not_exists_color: Color, // Command that doesn't exist (red)
    pub argument_color: Color,           // Arguments (cyan)
    pub variable_color: Color,           // Variables (yellow)
    pub single_quote_color: Color,       // Single quoted strings (green)
    pub double_quote_color: Color,       // Double quoted strings (Green with bold?)
    pub redirect_color: Color,           // Redirect operators (magenta)
    pub operator_color: Color,           // Logical/sequential operators
    pub pipe_color: Color,               // Pipe symbol
    pub background_color: Color,         // Background operator
    pub proc_subst_color: Color,         // Process substitution markers
    pub error_color: Color,              // Parse errors (red intense)
    pub completion_color: Color,         // Completion candidates (dark grey)
    pub ghost_color: Color,              // Inline suggestion text (dim gray)
    pub valid_path_color: Color,         // Valid path (magenta)
}

impl Default for InputConfig {
    fn default() -> InputConfig {
        InputConfig {
            fg_color: Color::White,
            command_exists_color: Color::Blue,
            command_not_exists_color: Color::Red,
            argument_color: Color::Cyan,
            variable_color: Color::Yellow,
            single_quote_color: Color::DarkGreen,
            double_quote_color: Color::Green,
            redirect_color: Color::Magenta,
            operator_color: Color::DarkYellow,
            pipe_color: Color::DarkCyan,
            background_color: Color::DarkMagenta,
            proc_subst_color: Color::DarkBlue,
            error_color: Color::Red,
            completion_color: Color::DarkGrey,
            ghost_color: Color::DarkGrey,
            valid_path_color: Color::Magenta,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Input {
    config: InputConfig,
    cursor: usize,
    input: String,
    indices: Vec<usize>,

    pub completion: Option<String>,
    pub color_ranges: Option<Vec<(usize, usize, ColorType)>>, // (start, end, color_type)
    pub can_execute: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ColorType {
    CommandExists,
    CommandNotExists,
    Argument,
    Variable,
    SingleQuote,
    DoubleQuote,
    Redirect,
    Operator,
    Pipe,
    Background,
    ProcSubst,
    Error,
    ValidPath,
}

impl Input {
    pub fn new(config: InputConfig) -> Input {
        Input {
            config,
            cursor: 0,
            input: String::with_capacity(INITIAL_CAP),
            indices: Vec::with_capacity(INITIAL_CAP),
            completion: None,
            color_ranges: None,
            can_execute: false,
        }
    }

    pub fn reset(&mut self, input: String) {
        self.input = input;
        self.update_indices();
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
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let remove_index = self.cursor - 1;
            let byte_index = self.indices[remove_index];
            let char_len = self.char_len_at(remove_index);

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

    /// Get the display width from the beginning to the cursor position
    pub fn cursor_display_width(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }

        let cursor_byte_pos = self.byte_index();
        let text_to_cursor = &self.input[..cursor_byte_pos];

        // Calculate width character by character for better control
        text_to_cursor
            .chars()
            .map(|c| c.width().unwrap_or_default())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub fn get_cursor_word(&self) -> Result<Option<(Rule, Span<'_>)>> {
        parser::get_pos_word(self.input.as_str(), self.cursor)
    }

    /// Fallback computation for completion word when parser cannot identify it (e.g. after redirects)
    pub fn get_completion_word_fallback(&self) -> Option<String> {
        if self.input.is_empty() || self.cursor == 0 {
            return None;
        }

        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.cursor;

        while start > 0 {
            let ch = chars[start - 1];
            if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '(' | ')' | '<' | '>') {
                break;
            }
            start -= 1;
        }

        if start < self.cursor {
            Some(chars[start..self.cursor].iter().collect())
        } else {
            None
        }
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

    /// Delete character at current cursor position (forward delete)
    pub fn delete_char(&mut self) {
        if self.cursor >= self.len() {
            return;
        }

        let byte_index = self.indices[self.cursor];
        let char_len = self.char_len_at(self.cursor);

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
            // Build colored segments to reduce write_fmt calls
            let colored_output = self.build_colored_string_from_ranges(color_ranges);
            writer.write_fmt(format_args!("{colored_output}")).ok();
        } else {
            writer
                .write_fmt(format_args!("{}", self.as_str().with(self.config.fg_color)))
                .ok();
        }

        if let Some(suffix) = ghost_suffix.filter(|s| !s.is_empty()) {
            writer
                .write_fmt(format_args!("{}", suffix.with(self.config.ghost_color)))
                .ok();
        }

        // Ensure all buffered output is written immediately
        writer.flush().ok();
    }

    /// Build a colored string from color ranges
    /// Note: color_ranges must be sorted by start position (ensured by compute_color_ranges)
    fn build_colored_string_from_ranges(
        &self,
        color_ranges: &[(usize, usize, ColorType)],
    ) -> String {
        use crossterm::style::Stylize;
        use std::fmt::Write;

        let input_str = self.as_str();
        // Pre-allocate capacity to reduce allocations (input + ANSI codes overhead)
        let mut result = String::with_capacity(input_str.len() * 2);
        let mut last_end = 0;

        // color_ranges is already sorted by start position in compute_color_ranges
        for &(start, end, color_type) in color_ranges {
            // Add any uncolored text before this range
            if start > last_end {
                let prefix = &input_str[last_end..start];
                let _ = write!(result, "{}", prefix.with(self.config.fg_color));
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
            let _ = write!(result, "{}", colored_text.with(color));

            // Update the last processed position
            last_end = end.max(last_end);
        }

        // Add any remaining uncolored text after the last range
        if last_end < input_str.len() {
            let suffix = &input_str[last_end..];
            let _ = write!(result, "{}", suffix.with(self.config.fg_color));
        }

        result
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

        // Build the complete output string to reduce write_fmt calls
        let mut output = String::new();
        output.push_str(&format!(
            "{}",
            completion.with(self.config.completion_color)
        ));

        if !is_end {
            let tmp = &self.input[current_byte..];
            output.push_str(&format!("{}", tmp.with(self.config.fg_color)));
        }

        // Single write operation
        writer.write_fmt(format_args!("{output}")).ok();

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

        // Character input test (using actual method names)
        input.insert('h');
        input.insert('i');
        assert_eq!(input.as_str(), "hi");
        assert_eq!(input.cursor(), 2);

        // Cursor movement test
        input.move_to_end();
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_unicode_width_calculation() {
        let config = InputConfig::default();
        let mut input = Input::new(config);

        // Test ASCII characters (width = 1 each)
        input.insert('a');
        input.insert('b');
        assert_eq!(input.cursor(), 2);
        assert_eq!(input.cursor_display_width(), 2);

        // Clear and test Japanese characters (width = 2 each)
        input.clear();
        input.insert('あ'); // Japanese hiragana 'a'
        input.insert('い'); // Japanese hiragana 'i'
        assert_eq!(input.cursor(), 2); // 2 characters
        assert_eq!(input.cursor_display_width(), 4); // 4 display width

        // Test mixed ASCII and Japanese
        input.clear();
        input.insert('a'); // width = 1
        input.insert('あ'); // width = 2
        input.insert('b'); // width = 1
        assert_eq!(input.cursor(), 3); // 3 characters
        assert_eq!(input.cursor_display_width(), 4); // 1 + 2 + 1 = 4 display width
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
        assert_eq!(input.cursor_display_width(), 0);

        input.move_by(1); // After 'a'
        assert_eq!(input.cursor(), 1);
        assert_eq!(input.cursor_display_width(), 1);

        input.move_by(1); // After 'あ'
        assert_eq!(input.cursor(), 2);
        assert_eq!(input.cursor_display_width(), 3); // 1 + 2

        input.move_by(1); // After 'b'
        assert_eq!(input.cursor(), 3);
        assert_eq!(input.cursor_display_width(), 4); // 1 + 2 + 1
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

    #[test]
    fn test_display_width_ansi() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(display_width("\x1b[1;32mbold green\x1b[0m"), 10);
        assert_eq!(display_width("normal \x1b[31mred\x1b[0m normal"), 17);
    }

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
    fn test_display_width() {
        // Test ASCII
        assert_eq!(display_width("hello"), 5);

        // Test Unicode
        assert_eq!(display_width("こんにちは"), 10); // 5 Japanese chars * 2 width each

        // Test emoji (dog emoji width is 2)
        let dog_width = display_width("🐕");
        assert_eq!(dog_width, 2);

        // Test mixed with ANSI codes
        let ansi_test = display_width("\x1b[31m🐕\x1b[0m < ");
        // emoji(2) + space(1) + <(1) + space(1) = 5
        assert_eq!(ansi_test, 5);

        // Test the actual prompt format
        let prompt_width = display_width("🐕 < ");
        // emoji(2) + space(1) + <(1) + space(1) = 5
        assert_eq!(prompt_width, 5);
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
        assert_eq!(input.cursor(), 4); // "one| two three" (wait, logic moves to end of word or start of next?)
        // Let's check logic:
        // skip non-spaces (current word) -> "one" skipped
        // skip spaces -> " " skipped
        // stops at "t" of "two"?
        // My implementation:
        // while idx < len && !chars[idx].is_whitespace() { idx += 1; } // skips "one", lands on space at 3
        // while idx < len && chars[idx].is_whitespace() { idx += 1; } // skips space, lands on 't' at 4
        assert_eq!(input.cursor(), 4);

        input.move_word_right();
        assert_eq!(input.cursor(), 8); // start of "three"

        input.move_word_right();
        assert_eq!(input.cursor(), 13); // end of string
    }
}
