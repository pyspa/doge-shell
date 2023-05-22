use crate::parser::{self, Rule};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use pest::Span;
use std::cmp::min;
use std::io::{BufWriter, StdoutLock, Write};

#[derive(Debug, Clone)]
pub struct Input {
    cursor: usize,
    input: String,
    indices: Vec<usize>,
    pub completion: Option<String>,
    pub match_index: Option<Vec<usize>>,
    pub can_execute: bool,
}

const INITIAL_CAP: usize = 256;

impl Input {
    pub fn new() -> Input {
        Input {
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

    pub fn to_string(&self) -> String {
        self.input.as_str().to_string()
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

    pub fn print(&self, out: &mut StdoutLock<'static>, fg_color: Color) {
        let mut out = BufWriter::new(out);

        if let Some(match_index) = &self.match_index {
            let mut index_iter = match_index.iter();
            let mut match_index = index_iter.next();

            for (i, ch) in self.as_str().chars().enumerate() {
                let color = if let Some(idx) = match_index {
                    if *idx == i {
                        match_index = index_iter.next();
                        Color::Blue
                    } else {
                        Color::White
                    }
                } else {
                    Color::White
                };

                out.write_fmt(format_args!("{}", ch.with(color))).ok();
            }
        } else {
            out.write_fmt(format_args!("{}", self.as_str().with(fg_color)))
                .ok();
        }
    }
}
