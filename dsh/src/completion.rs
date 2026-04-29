use crate::lisp::Value;
use dsh_frecency::ItemStats;
use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedSender;

pub mod cache;
pub mod command;
pub mod commands;
pub mod context;
pub mod display;
pub(crate) mod dynamic;
pub mod fuzzy;
pub mod path;
pub mod selection;
pub mod skim_adapter;

pub mod errors;
pub mod framework;
pub mod generator;
pub mod generators;
pub mod integrated;
pub mod json_loader;
pub mod parser;
mod ui;

#[cfg(test)]
mod extra_tests;
#[cfg(test)]
mod wrapped_tests;
#[cfg(test)]
mod z_tests;

// Re-export from completion module
pub use crate::completion::command::CompletionType;
pub use crate::completion::commands::{deduplicate_candidates, get_commands};
pub use crate::completion::display::Candidate;
pub use crate::completion::display::CompletionConfig;
pub use crate::completion::framework::CompletionSelection;
pub use crate::completion::fuzzy::fuzzy_match_score;
pub use crate::completion::path::*;
pub use crate::completion::selection::{
    completion_for_z, default_completion_framework, get_prompt_and_input_for_completion,
    input_completion, last_word, select_completion_items, select_completion_items_with_framework,
};
pub use crate::completion::skim_adapter::{replace_space, select_item_with_skim};

pub const MAX_RESULT: usize = 500;

#[derive(Debug, Clone)]
pub struct AutoComplete {
    pub target: String,
    pub cmd: Option<String>,
    pub func: Option<Value>,
    pub candidates: Option<Vec<String>>,
}

static COMPLETION_NOTIFIER: OnceLock<UnboundedSender<()>> = OnceLock::new();

pub fn set_completion_notifier(sender: UnboundedSender<()>) {
    let _ = COMPLETION_NOTIFIER.set(sender);
}

pub fn notify_completion_update() {
    if let Some(sender) = COMPLETION_NOTIFIER.get() {
        // UnboundedSender has send method which is non-blocking/synchronous
        let _ = sender.send(());
    }
}

/// Main completion structure
#[derive(Debug)]
pub struct Completion {
    pub input: Option<String>,
    pub current_index: usize,
    pub completions: Vec<ItemStats>,
}

impl Default for Completion {
    fn default() -> Self {
        Self::new()
    }
}

impl Completion {
    pub fn new() -> Self {
        Completion {
            input: None,
            current_index: 0,
            completions: Vec::new(),
        }
    }

    pub fn is_changed(&self, word: &str) -> bool {
        if let Some(input) = &self.input {
            input != word
        } else {
            !word.is_empty()
        }
    }

    pub fn clear(&mut self) {
        self.input = None;
        self.current_index = 0;
        self.completions = Vec::new();
    }

    pub fn completion_mode(&self) -> bool {
        !self.completions.is_empty()
    }

    pub fn set_completions(&mut self, input: &str, comps: Vec<ItemStats>) {
        let item = ItemStats::new(input, 0.0, 1.0, None);

        self.input = if input.is_empty() {
            None
        } else {
            Some(input.to_string())
        };
        self.completions = comps;
        self.completions.insert(0, item);
        self.current_index = 0;
    }

    pub fn backward(&mut self) -> Option<&ItemStats> {
        if self.completions.is_empty() {
            return None;
        }

        if self.completions.len() - 1 > self.current_index {
            self.current_index += 1;
            Some(&self.completions[self.current_index])
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<&ItemStats> {
        if self.current_index > 0 {
            self.current_index -= 1;
            Some(&self.completions[self.current_index])
        } else {
            None
        }
    }
}
