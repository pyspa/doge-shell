//! Terminal-safe presentation of untrusted completion data. Identity, matching,
//! insertion and renderer-owned escape sequences never pass through this view.
use std::borrow::Cow;

pub(crate) fn display_text(text: &str) -> Cow<'_, str> {
    if !text.chars().any(needs_escape) {
        return Cow::Borrowed(text);
    }
    let mut visible = String::with_capacity(text.len());
    for ch in text.chars() {
        if needs_escape(ch) {
            visible.extend(ch.escape_debug());
        } else {
            visible.push(ch);
        }
    }
    Cow::Owned(visible)
}

fn needs_escape(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')
}
