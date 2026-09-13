//! In-memory navigation over `History`: prefix-match Ctrl-P/Ctrl-N traversal (`back`/`forward`) and the lowercase cache that makes
//! case-insensitive search allocation-free.
use super::*;

impl History {
    pub(super) fn normalized_command(command: &str) -> String {
        command.to_lowercase()
    }

    pub(super) fn rebuild_normalized_entries(&mut self) {
        self.normalized_entries = self
            .histories
            .iter()
            .map(|entry| Self::normalized_command(&entry.entry))
            .collect();
    }

    fn search_is_case_sensitive(word: &str) -> bool {
        word.chars().any(|ch| ch.is_uppercase())
    }

    fn find_previous_match(&mut self, start: usize, word: &str) -> Option<(usize, String)> {
        let case_sensitive = Self::search_is_case_sensitive(word);
        let normalized_word = if case_sensitive {
            None
        } else {
            if self.normalized_entries.len() != self.histories.len() {
                self.rebuild_normalized_entries();
            }
            Some(Self::normalized_command(word))
        };
        let needle = normalized_word.as_deref().unwrap_or(word);

        for index in (0..=start).rev() {
            let haystack = if case_sensitive {
                self.histories[index].entry.as_str()
            } else {
                self.normalized_entries[index].as_str()
            };
            if haystack.contains(needle) {
                return Some((index, self.histories[index].entry.clone()));
            }
        }

        None
    }

    fn find_next_match(&mut self, start: usize, word: &str) -> Option<(usize, String)> {
        let case_sensitive = Self::search_is_case_sensitive(word);
        let normalized_word = if case_sensitive {
            None
        } else {
            if self.normalized_entries.len() != self.histories.len() {
                self.rebuild_normalized_entries();
            }
            Some(Self::normalized_command(word))
        };
        let needle = normalized_word.as_deref().unwrap_or(word);

        for index in start..self.histories.len() {
            let haystack = if case_sensitive {
                self.histories[index].entry.as_str()
            } else {
                self.normalized_entries[index].as_str()
            };
            if haystack.contains(needle) {
                return Some((index, self.histories[index].entry.clone()));
            }
        }

        None
    }

    fn get(&self, index: usize) -> Option<String> {
        if index < self.histories.len() {
            let entry = &self.histories[index].entry;
            Some(entry.to_string())
        } else {
            None
        }
    }

    /// Navigate backward through history.
    pub fn back(&mut self) -> Option<String> {
        if self.current_index == 0 {
            return None;
        }

        let start = self.current_index - 1;
        match self.search_word.clone() {
            Some(word) => {
                if let Some((index, entry)) = self.find_previous_match(start, &word) {
                    self.current_index = index;
                    Some(entry)
                } else {
                    None
                }
            }
            None => {
                self.current_index = start;
                self.get(self.current_index)
            }
        }
    }

    /// Navigate forward through history.
    pub fn forward(&mut self) -> Option<String> {
        let start = self.current_index + 1;
        if start >= self.histories.len() {
            if self.search_word.is_some() {
                self.reset_index();
            }
            return None;
        }

        match self.search_word.clone() {
            Some(word) => {
                if let Some((index, entry)) = self.find_next_match(start, &word) {
                    self.current_index = index;
                    Some(entry)
                } else {
                    self.reset_index();
                    None
                }
            }
            None => {
                self.current_index = start;
                self.get(self.current_index)
            }
        }
    }

    /// Reset history index to the end.
    pub fn reset_index(&mut self) {
        self.current_index = self.histories.len();
    }

    /// Check if at the end of history.
    pub fn at_end(&self) -> bool {
        self.current_index == self.histories.len()
    }

    /// Check if at the latest entry.
    pub fn at_latest_entry(&self) -> bool {
        self.current_index == self.histories.len().saturating_sub(1)
    }
}
