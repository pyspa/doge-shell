//! `{{placeholder}}` expansion for inserted snippets.
//!
//! A snippet body may carry `{{name}}` or `{{name:default}}` markers. On
//! insertion the markers are replaced by their defaults (empty when absent) and
//! the resulting positions become stops that `Alt+n` / `Alt+p` walk through.
//!
//! Offsets are counted in chars throughout, because that is what
//! [`crate::input::Input`] works in.

/// A `{{...}}` stop in the expanded text, as a char range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaceholderSpan {
    pub start: usize,
    pub end: usize,
}

/// Cursor stops left over from the most recent snippet insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaceholderState {
    spans: Vec<PlaceholderSpan>,
    current: usize,
}

impl PlaceholderState {
    /// Anchors `spans` (relative to the inserted text) at `offset` within the
    /// input buffer. Returns `None` when the snippet had no placeholders.
    pub(crate) fn new(spans: &[PlaceholderSpan], offset: usize) -> Option<Self> {
        if spans.is_empty() {
            return None;
        }
        Some(Self {
            spans: spans
                .iter()
                .map(|span| PlaceholderSpan {
                    start: span.start + offset,
                    end: span.end + offset,
                })
                .collect(),
            current: 0,
        })
    }

    /// Char offset of the stop the cursor should sit at right now.
    pub(crate) fn cursor(&self) -> usize {
        self.spans[self.current].start
    }

    /// Advances to the next stop, wrapping around. Wrapping keeps the key
    /// useful when you overshoot a three-field snippet.
    pub(crate) fn next(&mut self) -> usize {
        self.current = (self.current + 1) % self.spans.len();
        self.cursor()
    }

    pub(crate) fn prev(&mut self) -> usize {
        self.current = (self.current + self.spans.len() - 1) % self.spans.len();
        self.cursor()
    }

    /// Re-anchors the stops after an edit of `delta` chars made at char offset
    /// `at`.
    ///
    /// Without this, typing a value into the first placeholder would leave
    /// every later stop pointing at the wrong column — the whole point of the
    /// stops is to survive the text you type into them.
    ///
    /// A stop that starts after the edit slides; a stop the edit landed inside
    /// grows or shrinks instead, so the placeholder still spans what you typed.
    pub(crate) fn adjust(&mut self, at: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let shift = |value: usize| -> usize { (value as isize + delta).max(0) as usize };

        for span in &mut self.spans {
            if span.start > at {
                span.start = shift(span.start);
                span.end = shift(span.end);
            } else if span.end >= at {
                span.end = shift(span.end).max(span.start);
            }
        }
    }
}

/// Replaces `{{name}}` / `{{name:default}}` with the default text and reports
/// where each one ended up.
///
/// An unterminated `{{` is left alone rather than swallowing the rest of the
/// snippet.
pub(crate) fn expand(template: &str) -> (String, Vec<PlaceholderSpan>) {
    let mut out = String::with_capacity(template.len());
    let mut spans = Vec::new();
    // Char offset into `out`, which is what the caller needs.
    let mut out_chars = 0usize;

    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let (before, after_open) = rest.split_at(open);
        out.push_str(before);
        out_chars += before.chars().count();

        let body = &after_open[2..];
        let Some(close) = body.find("}}") else {
            // No closing marker: emit the rest verbatim.
            out.push_str(after_open);
            return (out, spans);
        };

        let default = match body[..close].split_once(':') {
            Some((_name, default)) => default,
            None => "",
        };
        out.push_str(default);
        let len = default.chars().count();
        spans.push(PlaceholderSpan {
            start: out_chars,
            end: out_chars + len,
        });
        out_chars += len;

        rest = &body[close + 2..];
    }
    out.push_str(rest);

    (out, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> PlaceholderSpan {
        PlaceholderSpan { start, end }
    }

    #[test]
    fn text_without_placeholders_is_unchanged() {
        let (text, spans) = expand("git status");
        assert_eq!(text, "git status");
        assert!(spans.is_empty());
    }

    #[test]
    fn bare_placeholder_collapses_to_an_empty_stop() {
        let (text, spans) = expand("git commit -m {{message}}");
        assert_eq!(text, "git commit -m ");
        assert_eq!(spans, vec![span(14, 14)]);
    }

    #[test]
    fn default_value_is_inserted_and_selected() {
        let (text, spans) = expand("docker run -it {{image:alpine}} sh");
        assert_eq!(text, "docker run -it alpine sh");
        assert_eq!(spans, vec![span(15, 21)]);
    }

    #[test]
    fn multiple_placeholders_report_shifted_offsets() {
        let (text, spans) = expand("scp {{src}} {{host:localhost}}:{{dst}}");
        assert_eq!(text, "scp  localhost:");
        assert_eq!(spans, vec![span(4, 4), span(5, 14), span(15, 15)]);
    }

    #[test]
    fn offsets_are_char_based_not_byte_based() {
        let (text, spans) = expand("echo 日本語 {{x:あ}}");
        assert_eq!(text, "echo 日本語 あ");
        // "echo 日本語 " is 9 chars, the default "あ" is 1.
        assert_eq!(spans, vec![span(9, 10)]);
    }

    #[test]
    fn unterminated_marker_is_left_verbatim() {
        let (text, spans) = expand("echo {{oops");
        assert_eq!(text, "echo {{oops");
        assert!(spans.is_empty());
    }

    #[test]
    fn state_anchors_spans_and_cycles_both_ways() {
        let spans = vec![span(0, 0), span(5, 9)];
        let mut state = PlaceholderState::new(&spans, 10).unwrap();

        assert_eq!(state.cursor(), 10);
        assert_eq!(state.next(), 15);
        // Wraps around rather than sticking at the end.
        assert_eq!(state.next(), 10);
        assert_eq!(state.prev(), 15);
    }

    #[test]
    fn no_spans_means_no_state() {
        assert!(PlaceholderState::new(&[], 0).is_none());
    }

    /// The failure this guards against: typing into the first placeholder used
    /// to leave the later stops pointing at their pre-edit columns.
    #[test]
    fn typing_into_a_stop_slides_the_later_ones() {
        // `scp {{src}} {{host:localhost}}:{{dst}}` expands to `scp  localhost:`
        let mut state = PlaceholderState::new(&[span(4, 4), span(5, 14), span(15, 15)], 0).unwrap();

        // Type "file.txt" (8 chars) at the first stop.
        state.adjust(4, 8);

        // The edited stop grew to cover what was typed...
        assert_eq!(state.cursor(), 4);
        // ...and the ones after it moved by the same amount.
        assert_eq!(state.next(), 13);
        assert_eq!(state.next(), 23);
    }

    #[test]
    fn deleting_pulls_the_later_stops_back() {
        let mut state = PlaceholderState::new(&[span(0, 5), span(10, 12)], 0).unwrap();
        state.adjust(0, -3);
        assert_eq!(state.cursor(), 0);
        assert_eq!(state.next(), 7);
    }

    #[test]
    fn edits_after_every_stop_change_nothing() {
        let mut state = PlaceholderState::new(&[span(0, 2), span(4, 6)], 0).unwrap();
        state.adjust(20, 5);
        assert_eq!(state.cursor(), 0);
        assert_eq!(state.next(), 4);
    }

    #[test]
    fn a_stop_never_shrinks_past_its_own_start() {
        let mut state = PlaceholderState::new(&[span(3, 5)], 0).unwrap();
        state.adjust(4, -100);
        // Cursor stays put rather than going negative or inverting the span.
        assert_eq!(state.cursor(), 3);
    }
}
