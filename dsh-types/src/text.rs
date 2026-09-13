//! Small, dependency-free string helpers shared across crates - nothing here
//! is specific to any one feature, which is why it lives in the leaf crate
//! rather than next to whichever caller happened to need it first.

/// Truncates `text` to at most `max_chars` characters (not bytes - a
/// criterion, a goal, or a table cell can be non-ASCII), marking that it was
/// cut. Used both for table cells (`dsh/src/cron/cli/render.rs`) and for
/// report sections (`dsh/src/agent/summary.rs`) - the two grew
/// byte-for-byte identical copies of this before it moved here.
pub fn clamp_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}...", &text[..cut]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_within_the_limit_is_unchanged() {
        assert_eq!(clamp_chars("hello", 10), "hello");
    }

    #[test]
    fn text_over_the_limit_is_cut_with_an_ellipsis() {
        assert_eq!(clamp_chars("hello world", 5), "hello...");
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        // Each "é" is 2 bytes; a byte-based cut would slice through one.
        let text = "éééééé";
        assert_eq!(clamp_chars(text, 3), "ééé...");
    }
}
