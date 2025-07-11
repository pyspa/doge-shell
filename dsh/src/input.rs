use crate::parser::{self, Rule};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use pest::Span;
use std::cmp::min;
use std::fmt;
use std::io::{BufWriter, StdoutLock, Write};

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

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub fn get_cursor_word(&self) -> Result<Option<(Rule, Span)>> {
        parser::get_pos_word(self.input.as_str(), self.cursor)
    }

    pub fn get_words(&self) -> Result<Vec<(Rule, Span, bool)>> {
        parser::get_words(self.input.as_str(), self.cursor)
    }

    pub fn print(&self, out: &mut StdoutLock<'static>) {
        let mut out = BufWriter::new(out);

        if let Some(match_index) = &self.match_index {
            // Build colored segments to reduce write_fmt calls
            let colored_output = self.build_colored_string(match_index);
            out.write_fmt(format_args!("{}", colored_output)).ok();
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
        let current = self.cursor;
        let length = self.input.len();
        let is_end = current == length;

        // Build the complete output string to reduce write_fmt calls
        let mut output = String::new();
        output.push_str(&format!(
            "{}",
            completion.with(self.config.completion_color)
        ));

        if !is_end {
            let tmp = &self.input[current..];
            output.push_str(&format!("{}", tmp.with(self.config.fg_color)));
        }

        // Single write operation
        out.write_fmt(format_args!("{}", output)).ok();

        // Ensure all buffered output is written immediately
        out.flush().ok();
    }

    pub fn split_current_pos(&self) -> Option<(&str, &str)> {
        let current = self.cursor;
        let length = self.input.len();
        let is_end = current == length;
        if is_end {
            None
        } else {
            let pre = &self.input[..current];
            let post = &self.input[current..];
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
        assert_eq!(format!("{}", input), "");
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
}
