use crate::parser::{self, Rule, ShellParser};
use anyhow::{anyhow, Result};
use pest::iterators::{Pair, Pairs};
use pest::Parser;
use std::cmp::min;

#[derive(Debug, Clone)]
pub struct Input {
    cursor: usize,
    input: String,
    indices: Vec<usize>,
    pub completion: Option<String>,
}

const INITIAL_CAP: usize = 256;

impl Input {
    pub fn new() -> Input {
        Input {
            cursor: 0,
            input: String::with_capacity(INITIAL_CAP),
            indices: Vec::with_capacity(INITIAL_CAP),
            completion: None,
        }
    }

    pub fn reset(&mut self, input: String) {
        self.input = input;
        self.update_indices();
        self.move_to_end();
    }

    pub fn as_str(&self) -> String {
        self.input.as_str().to_string()
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

    pub fn move_by(&mut self, offset: isize) {
        if offset < 0 {
            self.cursor = self.cursor.saturating_sub(offset.abs() as usize);
        } else {
            self.cursor = min(self.len(), self.cursor + offset.abs() as usize);
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

    pub fn get_cursor_word(&self) -> Result<Option<(Rule, String)>> {
        parser::get_pos_word(self.input.as_str(), self.cursor)
    }
}
