//! Reading words out of the input line: the parser-backed lookups used by
//! completion (`get_cursor_word`/`get_words`), the shell-token fallback for
//! positions the parser cannot name, and the whitespace-only scan abbreviation
//! expansion uses (`get_current_word_for_abbr`/`replace_current_word`).
use super::*;

impl Input {
    pub fn get_cursor_word(&self) -> Result<Option<(Rule, Span<'_>)>> {
        parser::get_pos_word(self.input.as_str(), self.cursor)
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

    pub fn get_words(&self) -> Result<Vec<(Rule, Span<'_>, bool)>> {
        parser::get_words(self.input.as_str(), self.cursor)
    }

    pub fn get_words_from_pairs<'a>(&self, pairs: Pairs<'a, Rule>) -> Vec<(Rule, Span<'a>, bool)> {
        parser::get_words_from_pairs(pairs, self.cursor)
    }
}
