#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeparatorMode {
    Parser,
    CompletionRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellTokenSpan {
    pub raw: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub char_start: usize,
    pub char_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    Unquoted,
    Single,
    Double,
}

pub(crate) fn tokenize(input: &str, mode: SeparatorMode) -> Vec<ShellTokenSpan> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut token_start: Option<(usize, usize)> = None;
    let mut quote = QuoteState::Unquoted;
    let mut escaped = false;
    let char_count = input.chars().count();

    for (char_index, (byte_index, ch)) in input.char_indices().enumerate() {
        if escaped {
            ensure_token_start(&mut token_start, byte_index, char_index);
            current.push(ch);
            escaped = false;
            continue;
        }

        match quote {
            QuoteState::Unquoted => {
                if is_separator(ch, mode) {
                    push_current_span(
                        &mut spans,
                        &mut current,
                        &mut token_start,
                        byte_index,
                        char_index,
                    );
                    continue;
                }

                ensure_token_start(&mut token_start, byte_index, char_index);
                current.push(ch);
                match ch {
                    '\\' => escaped = true,
                    '\'' => quote = QuoteState::Single,
                    '"' => quote = QuoteState::Double,
                    _ => {}
                }
            }
            QuoteState::Single => {
                ensure_token_start(&mut token_start, byte_index, char_index);
                current.push(ch);
                if ch == '\'' {
                    quote = QuoteState::Unquoted;
                }
            }
            QuoteState::Double => {
                ensure_token_start(&mut token_start, byte_index, char_index);
                current.push(ch);
                match ch {
                    '\\' => escaped = true,
                    '"' => quote = QuoteState::Unquoted,
                    _ => {}
                }
            }
        }
    }

    push_current_span(
        &mut spans,
        &mut current,
        &mut token_start,
        input.len(),
        char_count,
    );

    spans
}

pub(crate) fn token_at_char_cursor(
    input: &str,
    cursor_pos: usize,
    mode: SeparatorMode,
) -> Option<ShellTokenSpan> {
    let char_count = input.chars().count();
    let cursor = cursor_pos.min(char_count);

    tokenize(input, mode)
        .into_iter()
        .find(|span| cursor >= span.char_start && cursor <= span.char_end)
}

fn ensure_token_start(
    token_start: &mut Option<(usize, usize)>,
    byte_index: usize,
    char_index: usize,
) {
    if token_start.is_none() {
        *token_start = Some((byte_index, char_index));
    }
}

fn push_current_span(
    spans: &mut Vec<ShellTokenSpan>,
    current: &mut String,
    token_start: &mut Option<(usize, usize)>,
    byte_end: usize,
    char_end: usize,
) {
    let Some((byte_start, char_start)) = token_start.take() else {
        return;
    };

    if current.is_empty() {
        return;
    }

    spans.push(ShellTokenSpan {
        raw: std::mem::take(current),
        byte_start,
        byte_end,
        char_start,
        char_end,
    });
}

fn is_separator(ch: char, mode: SeparatorMode) -> bool {
    match mode {
        SeparatorMode::Parser => matches!(ch, ' ' | '\t'),
        SeparatorMode::CompletionRange => {
            ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '(' | ')' | '<' | '>')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raws(spans: Vec<ShellTokenSpan>) -> Vec<String> {
        spans.into_iter().map(|span| span.raw).collect()
    }

    #[test]
    fn tokenize_preserves_backslash_escaped_whitespace() {
        let spans = tokenize(r#"cat dir\ with\ space/fo"#, SeparatorMode::Parser);
        assert_eq!(raws(spans), vec!["cat", r#"dir\ with\ space/fo"#]);
    }

    #[test]
    fn tokenize_preserves_quoted_and_unclosed_tokens() {
        assert_eq!(
            raws(tokenize(r#"cmd 'a b' "c d" e"#, SeparatorMode::Parser)),
            vec!["cmd", "'a b'", r#""c d""#, "e"]
        );
        assert_eq!(
            raws(tokenize(r#"echo "hello"#, SeparatorMode::Parser)),
            vec!["echo", r#""hello"#]
        );
    }

    #[test]
    fn escaped_quote_does_not_enter_quote_state() {
        let spans = tokenize(r#"echo \"quoted tail"#, SeparatorMode::Parser);
        assert_eq!(raws(spans), vec!["echo", r#"\"quoted"#, "tail"]);
    }

    #[test]
    fn parser_mode_does_not_split_shell_operators() {
        let spans = tokenize("cmd a|b", SeparatorMode::Parser);
        assert_eq!(raws(spans), vec!["cmd", "a|b"]);
    }

    #[test]
    fn completion_range_mode_splits_shell_operators() {
        let spans = tokenize("cmd a|b", SeparatorMode::CompletionRange);
        assert_eq!(raws(spans), vec!["cmd", "a", "b"]);
    }

    #[test]
    fn spans_keep_byte_and_char_positions_separate() {
        let spans = tokenize("cmd あ b", SeparatorMode::Parser);
        assert_eq!(
            spans[1],
            ShellTokenSpan {
                raw: "あ".to_string(),
                byte_start: 4,
                byte_end: 7,
                char_start: 4,
                char_end: 5,
            }
        );
    }

    #[test]
    fn token_at_char_cursor_returns_complete_token() {
        let token = token_at_char_cursor(
            r#"cat "dir with space/fo"#,
            18,
            SeparatorMode::CompletionRange,
        )
        .unwrap();
        assert_eq!(token.raw, r#""dir with space/fo"#);
        assert_eq!(token.char_start, 4);
        assert_eq!(token.char_end, 22);
    }
}
