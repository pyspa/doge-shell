use pest_derive::Parser;

#[derive(Parser, Debug, Clone)]
#[grammar = "shell.pest"]
pub struct ShellParser;

pub mod ast;
pub mod expansion;
pub mod highlight;

#[cfg(test)]
mod edge_case_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_escape;

// Re-exports
pub use ast::{get_pos_word, get_string, get_words, get_words_from_pairs};
pub use expansion::{expand_alias, parse_with_expansion};
pub use highlight::{
    HighlightKind, HighlightResult, HighlightToken, collect_highlight_tokens_from_pairs,
    highlight_error_token,
};
pub mod check;
pub use check::is_incomplete_input;

/// The portion of `input` the parser did not consume, or `None` when only
/// whitespace is left.
///
/// The grammar deliberately has no `EOI` anchor — highlighting and completion
/// must keep parsing half-typed lines — so `Rule::commands` can return a
/// partial match. Callers that are about to *act* on a parse (running it,
/// flagging it in the input line) use this to notice the leftover instead of
/// silently dropping it.
///
/// `consumed` is a byte offset from pest. An offset that lands inside a
/// multi-byte character is rounded *down*: reporting a little extra text is
/// right, while treating the line as fully consumed would silently restore the
/// very failure this guard exists to catch.
pub fn unparsed_tail(input: &str, consumed: usize) -> Option<&str> {
    let rest = input[unparsed_tail_start(input, consumed)..].trim();
    (!rest.is_empty()).then_some(rest)
}

/// Where the leftover begins, rounded down to a character boundary.
///
/// Callers that slice `input` must use this rather than the raw offset: a byte
/// index inside a multi-byte character panics on the way to the screen.
pub fn unparsed_tail_start(input: &str, consumed: usize) -> usize {
    let mut start = consumed.min(input.len());
    while start > 0 && !input.is_char_boundary(start) {
        start -= 1;
    }
    start
}

#[cfg(test)]
mod unparsed_tail_tests {
    use super::unparsed_tail;

    #[test]
    fn none_when_everything_is_consumed() {
        assert_eq!(unparsed_tail("echo hello", "echo hello".len()), None);
    }

    #[test]
    fn ignores_trailing_whitespace() {
        assert_eq!(unparsed_tail("echo hi  ", 7), None);
    }

    #[test]
    fn reports_leftover_text() {
        // The grammar stops at `]`, so everything from there on is dropped.
        assert_eq!(unparsed_tail("echo a]b", 6), Some("]b"));
    }

    #[test]
    fn tolerates_an_offset_past_the_end() {
        assert_eq!(unparsed_tail("echo", 99), None);
    }

    #[test]
    fn rounds_a_mid_character_offset_down() {
        // Offset 6 lands inside the 3-byte character starting at 5. Rounding
        // down still reports the leftover; returning `None` would mean a line
        // ending in a multi-byte word was silently truncated.
        assert_eq!(unparsed_tail("echo \u{3042}", 6), Some("\u{3042}"));
    }

    #[test]
    fn an_offset_at_a_character_boundary_is_exact() {
        assert_eq!(unparsed_tail("echo \u{3042}", 5), Some("\u{3042}"));
    }
}
