/// Calculate the actual display width of a string, accounting for ANSI codes and Unicode width
pub fn display_width(input: &str) -> usize {
    let mut width = 0;
    let mut in_ansi_sequence = false;
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        if in_ansi_sequence {
            // End of ANSI sequence is usually a letter
            if ch.is_ascii_alphabetic() {
                in_ansi_sequence = false;
            }
            continue;
        }

        if ch == '\x1b' {
            // Check next char to confirm CSI sequence
            // We peak by cloning iterator or just consuming one more
            // Since we are in a loop, we can just check the next char.
            // But we need to be careful not to consume it if it's not part of sequence (unlikely for \x1b)
            // Ideally we check if next is '['

            // For simple ANSI stripping: \x1b followed by [ ... letter
            // We just assume it's an escape sequence start.
            // Let's peek the next char if possible, or just consume it.
            // Since we don't have peekable here without allocation/wrapping,
            // we'll use a slightly different approach or just consume.
            // However, `chars` is an iterator.
            // Let's try to be robust.

            // We can consume the next char.
            if let Some(next) = chars.next() {
                if next == '[' {
                    in_ansi_sequence = true;
                } else {
                    // It was just an ESC char? Treat as 0 width non-printable or 1?
                    // Usually ESC is non-printable.
                }
            }
        } else {
            width += unicode_width::UnicodeWidthChar::width_cjk(ch).unwrap_or(0);
        }
    }
    width
}

#[cfg(test)]
mod display_width_tests {
    use super::*;

    #[test]
    fn test_display_width_special_chars() {
        // "✘" is reported as 1 by unicode-width even in CJK mode, but often renders as 2.
        // The safety margin in prompt.rs handles this discrepancy.
        // We verify that width_cjk is active by checking a standard CJK character.
        assert_eq!(display_width("あ"), 2, "CJK character should be width 2");
        assert_eq!(display_width("✘"), 1, "Library reports 1 for ✘");
    }
}
