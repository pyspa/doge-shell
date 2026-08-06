//! `Alt+.` — insert the last argument of a previous command.
//!
//! readline calls this `insert-last-argument`. Pressing the key repeatedly walks
//! further back through history, each press replacing what the previous one
//! inserted.

use crate::completion::shell_token::{SeparatorMode, tokenize};

/// The in-flight state of an `Alt+.` run.
///
/// Lives in [`crate::repl::state::ReplState`] and is cleared as soon as any
/// other action runs, so the next `Alt+.` starts over from the newest command.
#[derive(Debug, Clone)]
pub(crate) struct LastArgState {
    /// Extracted last arguments, newest first.
    candidates: Vec<String>,
    /// Index of the candidate to insert on the *next* press.
    next_index: usize,
    /// Char range currently occupied by the inserted text.
    start: usize,
    end: usize,
}

impl LastArgState {
    /// Builds the candidate list from history entries ordered newest first.
    ///
    /// Adjacent duplicates are collapsed: history stores one row per distinct
    /// command, but different commands frequently end in the same argument
    /// (`vim foo.rs`, `cargo check foo.rs`) and offering it twice in a row just
    /// makes the key feel broken. Non-adjacent repeats are kept — they carry
    /// real information about how far back you are.
    pub(crate) fn new(commands: impl IntoIterator<Item = String>, cursor: usize) -> Option<Self> {
        let mut candidates: Vec<String> = Vec::new();
        for command in commands {
            if let Some(arg) = last_argument(&command)
                && candidates.last() != Some(&arg)
            {
                candidates.push(arg);
            }
        }
        if candidates.is_empty() {
            return None;
        }
        Some(Self {
            candidates,
            next_index: 0,
            start: cursor,
            end: cursor,
        })
    }

    /// Returns the next candidate along with the char range it should replace,
    /// or `None` once the history is exhausted.
    pub(crate) fn advance(&mut self) -> Option<(usize, usize, String)> {
        let candidate = self.candidates.get(self.next_index)?.clone();
        let range = (self.start, self.end);
        self.next_index += 1;
        self.end = self.start + candidate.chars().count();
        Some((range.0, range.1, candidate))
    }
}

/// Extracts the last argument of `command`, preserving the original quoting.
///
/// The raw source slice is returned rather than the unquoted token so that
/// `"a b"` comes back with its quotes and stays a single argument when
/// re-executed.
pub(crate) fn last_argument(command: &str) -> Option<String> {
    let spans = tokenize(command, SeparatorMode::Parser);
    let last = spans.last()?;
    let text = &command[last.byte_start..last.byte_end];
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_final_token() {
        assert_eq!(last_argument("git commit -m wip"), Some("wip".to_string()));
        assert_eq!(last_argument("ls -la /tmp"), Some("/tmp".to_string()));
    }

    #[test]
    fn a_lone_command_is_its_own_last_argument() {
        // Matches bash's `!$` on a single-word command.
        assert_eq!(last_argument("ls"), Some("ls".to_string()));
    }

    #[test]
    fn preserves_quoting_so_the_argument_stays_one_word() {
        assert_eq!(
            last_argument(r#"git commit -m "hello world""#),
            Some(r#""hello world""#.to_string())
        );
        assert_eq!(
            last_argument("awk '{print $1}'"),
            Some("'{print $1}'".to_string())
        );
    }

    #[test]
    fn ignores_trailing_whitespace() {
        assert_eq!(last_argument("cargo build   "), Some("build".to_string()));
    }

    #[test]
    fn blank_input_has_no_last_argument() {
        assert_eq!(last_argument(""), None);
        assert_eq!(last_argument("   "), None);
    }

    #[test]
    fn state_walks_backwards_replacing_the_previous_insert() {
        let history = vec![
            "vim src/main.rs".to_string(),
            "cargo test".to_string(),
            "cd /tmp".to_string(),
        ];
        let mut state = LastArgState::new(history, 5).unwrap();

        assert_eq!(state.advance(), Some((5, 5, "src/main.rs".to_string())));
        // The second press replaces exactly what the first one inserted.
        assert_eq!(state.advance(), Some((5, 16, "test".to_string())));
        assert_eq!(state.advance(), Some((5, 9, "/tmp".to_string())));
        assert_eq!(state.advance(), None);
    }

    #[test]
    fn collapses_adjacent_duplicate_arguments() {
        let history = vec![
            "vim foo.rs".to_string(),
            "cargo check foo.rs".to_string(),
            "cd /tmp".to_string(),
            "ls foo.rs".to_string(),
        ];
        let mut state = LastArgState::new(history, 0).unwrap();

        assert_eq!(
            state.advance().map(|(_, _, arg)| arg),
            Some("foo.rs".into())
        );
        assert_eq!(state.advance().map(|(_, _, arg)| arg), Some("/tmp".into()));
        // Non-adjacent repeat is kept: it means something different this far back.
        assert_eq!(
            state.advance().map(|(_, _, arg)| arg),
            Some("foo.rs".into())
        );
        assert_eq!(state.advance(), None);
    }

    #[test]
    fn empty_history_yields_no_state() {
        assert!(LastArgState::new(Vec::new(), 0).is_none());
        assert!(LastArgState::new(vec!["   ".to_string()], 0).is_none());
    }
}
