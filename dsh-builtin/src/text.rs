//! Shared small text-formatting helpers for list/preview output.
//!
//! `bookmark` and `snippet` each render a one-line table of saved commands
//! and each want the same "don't let one entry blow out the column width"
//! truncation. This is that one implementation.

/// Truncates `text` to `max_chars` with a trailing `...` once it is over 50
/// characters; otherwise returns it unchanged.
pub(crate) fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() > 50 {
        let prefix: String = text.chars().take(max_chars).collect();
        format!("{prefix}...")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_preview_judges_multibyte_text_by_char_count() {
        // 30 multi-byte characters is under the 50-char threshold by count,
        // even though it is well over 50 bytes.
        let text = "あ".repeat(30);
        assert_eq!(truncate_preview(&text, 47), text);
    }

    #[test]
    fn truncate_preview_shortens_and_marks_truncation() {
        let text = "a".repeat(60);
        let result = truncate_preview(&text, 47);
        assert_eq!(result, format!("{}...", "a".repeat(47)));
    }
}
