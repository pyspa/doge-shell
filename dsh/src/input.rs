use crate::parser::{self, Rule};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use pest::Span;
use std::cmp::min;
use std::fmt;
use std::io::{BufWriter, StdoutLock, Write};
use unicode_width::UnicodeWidthChar;

/// Remove ANSI escape sequences from a string and return the clean string
fn strip_ansi_codes(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Found escape sequence, skip until we find the end
            if chars.next() == Some('[') {
                // Skip until we find a letter (end of ANSI sequence)
                for next_ch in chars.by_ref() {
                    if next_ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Calculate the actual display width of a string, accounting for ANSI codes and Unicode width
pub fn display_width(input: &str) -> usize {
    let clean_str = strip_ansi_codes(input);

    // Use UnicodeWidthStr::width_cjk for better East Asian character support
    // This treats ambiguous-width characters as wide (2 columns)
    clean_str
        .chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or_default())
        .sum()
}

const INITIAL_CAP: usize = 256;

#[derive(Debug, Clone)]
pub struct InputConfig {
    pub fg_color: Color,
    pub match_color: Color,
    pub completion_color: Color,
}

impl Default for InputConfig {
    fn default() -> InputConfig {
        InputConfig {
            fg_color: Color::White,
            match_color: Color::Blue,
            completion_color: Color::DarkGrey,
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
    pub match_index: Option<Vec<usize>>,
    pub can_execute: bool,
}

impl Input {
    pub fn new(config: InputConfig) -> Input {
        Input {
            config,
            cursor: 0,
            input: String::with_capacity(INITIAL_CAP),
            indices: Vec::with_capacity(INITIAL_CAP),
            completion: None,
            match_index: None,
            can_execute: false,
        }
    }

    pub fn reset(&mut self, input: String) {
        self.input = input;
        self.update_indices();
        self.move_to_end();
        self.match_index = None;
    }

    pub fn reset_with_match_index(&mut self, input: String, match_index: Vec<usize>) {
        self.input = input;
        self.update_indices();
        self.move_to_end();
        self.match_index = Some(match_index);
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
    }

    pub fn move_to_begin(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor = self.len();
    }

    pub fn insert(&mut self, ch: char) {
        self.input.insert(self.byte_index(), ch);
        self.update_indices();
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, string: &str) {
        self.input.insert_str(self.byte_index(), string);
        self.update_indices();
        self.cursor += string.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.byte_index());
            self.update_indices();
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

    fn byte_index(&self) -> usize {
        if self.cursor == self.indices.len() {
            self.input.len()
        } else {
            self.indices[self.cursor]
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
        let width = text_to_cursor
            .chars()
            .map(|c| c.width().unwrap_or_default())
            .sum();

        // Debug output for troubleshooting
        tracing::debug!(
            "cursor_display_width: cursor={}, byte_pos={}, text='{}', width={}",
            self.cursor,
            cursor_byte_pos,
            text_to_cursor,
            width
        );

        width
    }

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub fn get_cursor_word(&self) -> Result<Option<(Rule, Span)>> {
        parser::get_pos_word(self.input.as_str(), self.cursor)
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
        if self.cursor < self.len() {
            self.input.remove(self.byte_index());
            self.update_indices();
        }
    }

    pub fn get_words(&self) -> Result<Vec<(Rule, Span, bool)>> {
        parser::get_words(self.input.as_str(), self.cursor)
    }

    pub fn print(&self, out: &mut StdoutLock<'static>) {
        let mut out = BufWriter::new(out);

        if let Some(match_index) = &self.match_index {
            // Build colored segments to reduce write_fmt calls
            let colored_output = self.build_colored_string(match_index);
            out.write_fmt(format_args!("{colored_output}")).ok();
        } else {
            out.write_fmt(format_args!("{}", self.as_str().with(self.config.fg_color)))
                .ok();
        }

        // Ensure all buffered output is written immediately
        out.flush().ok();
    }

    /// Build a colored string by grouping consecutive characters with the same color
    fn build_colored_string(&self, match_index: &[usize]) -> String {
        use crossterm::style::Stylize;

        let mut result = String::new();
        let mut current_segment = String::new();
        let mut current_color = self.config.fg_color;
        let mut match_iter = match_index.iter();
        let mut next_match_idx = match_iter.next();

        for (i, ch) in self.as_str().chars().enumerate() {
            let color = if let Some(&idx) = next_match_idx {
                if idx == i {
                    next_match_idx = match_iter.next();
                    self.config.match_color
                } else {
                    self.config.fg_color
                }
            } else {
                self.config.fg_color
            };

            // If color changes, flush current segment and start new one
            if color != current_color {
                if !current_segment.is_empty() {
                    result.push_str(&format!("{}", current_segment.clone().with(current_color)));
                    current_segment.clear();
                }
                current_color = color;
            }

            current_segment.push(ch);
        }

        // Flush remaining segment
        if !current_segment.is_empty() {
            result.push_str(&format!("{}", current_segment.with(current_color)));
        }

        result
    }

    #[allow(dead_code)]
    pub fn fg_color(&self) -> Color {
        self.config.fg_color
    }

    #[allow(dead_code)]
    pub fn match_color(&self) -> Color {
        self.config.match_color
    }

    #[allow(dead_code)]
    pub fn completion_color(&self) -> Color {
        self.config.completion_color
    }

    pub fn print_candidates(&mut self, out: &mut StdoutLock<'static>, completion: String) {
        let mut out = BufWriter::new(out);
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
        out.write_fmt(format_args!("{output}")).ok();

        // Ensure all buffered output is written immediately
        out.flush().ok();
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
    fn test_strip_ansi_codes() {
        // Test plain text
        assert_eq!(strip_ansi_codes("hello"), "hello");

        // Test text with ANSI color codes
        assert_eq!(strip_ansi_codes("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(
            strip_ansi_codes("\x1b[1;32mbold green\x1b[0m"),
            "bold green"
        );

        // Test mixed content
        assert_eq!(
            strip_ansi_codes("normal \x1b[31mred\x1b[0m normal"),
            "normal red normal"
        );
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
}
