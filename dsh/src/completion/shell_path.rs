//! Shell-word insertion and quote-style normalization for completion.
//! Providers and caches carry raw literal words (e.g. `hello world`); explicit
//! built-in metadata identifies variable/home syntax. Prepared candidates keep
//! display/preview values raw while selectors output encoded source. Legacy
//! direct Lisp String replacements already contain source and bypass this step.
use crate::completion::display::{Candidate, ShellSyntaxKind};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellPathStyle {
    Plain,
    Escaped,
    SingleQuoted { closed: bool },
    DoubleQuoted { closed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellPathToken {
    normalized: String,
    style: ShellPathStyle,
}

impl ShellPathToken {
    pub fn from_raw(raw: &str) -> Self {
        let style = detect_style(raw);
        let normalized = decode_literal_word(raw);

        Self { normalized, style }
    }

    #[cfg(test)]
    fn normalized(&self) -> &str {
        &self.normalized
    }

    fn encode_syntax(&self, text: &str, kind: ShellSyntaxKind) -> String {
        match self.style {
            // Variables expand inside double quotes; homes do not. Single
            // quotes intentionally make both literal, preserving quote context.
            ShellPathStyle::DoubleQuoted { closed } if kind == ShellSyntaxKind::Variable => {
                format!("\"{text}{}", if closed { "\"" } else { "" })
            }
            ShellPathStyle::Plain | ShellPathStyle::Escaped => text.to_string(),
            _ => self.encode(text),
        }
    }

    fn encode_home_path(&self, path: &str) -> String {
        if matches!(self.style, ShellPathStyle::Plain | ShellPathStyle::Escaped)
            && let Some(tail) = path.strip_prefix("~/")
        {
            format!("~/{}", escape_unquoted(tail))
        } else {
            self.encode(path)
        }
    }

    pub fn encode(&self, path: &str) -> String {
        match self.style {
            ShellPathStyle::Plain | ShellPathStyle::Escaped => escape_unquoted(path),
            ShellPathStyle::SingleQuoted { closed } => {
                // A trailing backslash would escape the closing single quote
                // in doge's grammar. Double quotes can represent it faithfully.
                if path.ends_with('\\') {
                    return format!(
                        "\"{}{}",
                        escape_double_quoted(path),
                        if closed { "\"" } else { "" }
                    );
                }
                let escaped = escape_single_quoted(path);
                if closed {
                    format!("'{}'", escaped)
                } else {
                    format!("'{}", escaped)
                }
            }
            ShellPathStyle::DoubleQuoted { closed } => {
                let escaped = escape_double_quoted(path);
                if closed {
                    format!("\"{}\"", escaped)
                } else {
                    format!("\"{}", escaped)
                }
            }
        }
    }
}

pub(crate) fn normalize_path_token(raw: &str) -> String {
    ShellPathToken::from_raw(raw).normalized
}

pub(crate) fn format_path_for_token(path: &str, raw_token: &str) -> String {
    let token = ShellPathToken::from_raw(raw_token);
    // This entry is a path producer with the original source token, not an
    // arbitrary external candidate. Only unquoted `~/` source requests expand.
    if raw_token.starts_with("~/") {
        token.encode_home_path(path)
    } else {
        token.encode(path)
    }
}

pub fn format_candidates_for_token(
    candidates: Vec<Candidate>,
    raw_token: Option<&str>,
) -> Vec<Candidate> {
    // An empty token still needs a style so that candidates carrying spaces are
    // escaped when completion starts on a fresh word.
    let token = ShellPathToken::from_raw(raw_token.unwrap_or_default());

    candidates
        .into_iter()
        .map(|candidate| {
            // Formatting is idempotent: caches contain raw candidates, selectors
            // receive Prepared candidates. Display and previews retain raw values.
            if matches!(
                candidate,
                Candidate::Prepared { .. } | Candidate::History { .. }
            ) {
                return candidate;
            }
            let insertion = match &candidate {
                Candidate::HomePath(candidate) => {
                    token.encode_home_path(candidate.get_display_name())
                }
                Candidate::ShellSyntax { kind, text, .. } => token.encode_syntax(text, *kind),
                _ => token.encode(candidate.get_display_name()),
            };
            Candidate::Prepared {
                candidate: Box::new(candidate),
                insertion,
            }
        })
        .collect()
}

fn detect_style(raw: &str) -> ShellPathStyle {
    for quote in ['"', '\''] {
        if raw.starts_with(quote)
            && let Some(closed) = wrapping_quote(raw, quote)
        {
            return if quote == '"' {
                ShellPathStyle::DoubleQuoted { closed }
            } else {
                ShellPathStyle::SingleQuoted { closed }
            };
        }
    }
    if raw.contains('\\') {
        ShellPathStyle::Escaped
    } else {
        ShellPathStyle::Plain
    }
}

/// Preserve quote style only when one quote wraps the entire word (or is
/// unfinished). `"he"llo` is a concatenation, not an unfinished double quote.
fn wrapping_quote(raw: &str, quote: char) -> Option<bool> {
    let mut chars = raw.char_indices().skip(1).peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch == '\\'
            && chars.peek().is_some_and(|(_, next)| {
                if quote == '\'' {
                    *next == '\''
                } else {
                    matches!(next, '$' | '"' | '\\' | '`')
                }
            })
        {
            chars.next();
        } else if ch == quote {
            return (offset + ch.len_utf8() == raw.len()).then_some(true);
        }
    }
    Some(false)
}

/// Decode literal source spelling only. Never expand variables, globs or
/// substitutions. Adjacent quoted/unquoted parts form one search word; an
/// unfinished quote is accepted at the cursor just like a complete quote.
pub(crate) fn decode_literal_word(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut quote = None;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let escaped = chars.peek().is_some_and(|next| match quote {
                Some('\'') => *next == '\'',
                Some('"') => matches!(next, '$' | '"' | '\\' | '`'),
                _ => true,
            });
            if escaped {
                result.push(chars.next().unwrap());
            } else {
                result.push(ch);
            }
        } else if Some(ch) == quote {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else {
            result.push(ch);
        }
    }
    result
}

fn escape_unquoted(path: &str) -> String {
    if path.is_empty() {
        return "''".to_string();
    }
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_whitespace()
            || matches!(
                ch,
                '\\' | '"'
                    | '\''
                    | '|'
                    | '&'
                    | ';'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '$'
                    | '`'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '#'
                    | '~'
                    | '='
            )
        {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn escape_single_quoted(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch == '\'' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn escape_double_quoted(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_style_does_not_treat_escaped_quotes_as_closing_delimiters() {
        assert_eq!(normalize_path_token(r#"'can\'"#), "can'");
        assert_eq!(format_path_for_token("can't", r#"'can\'"#), r#"'can\'t"#);
        assert_eq!(normalize_path_token(r#""a\""#), "a\"");
        assert_eq!(format_path_for_token("a\"b", r#""a\""#), r#""a\"b"#);
    }

    #[test]
    fn literal_candidates_escape_independent_of_description() {
        use skim::SkimItem;
        for value in [
            "hello world",
            "a;b",
            "*.rs",
            "[ab]",
            "x{a,b}",
            "#tag",
            "~root",
            "",
            "日本語 e\u{301}",
        ] {
            let mut outputs = Vec::new();
            for candidate in [
                Candidate::Basic(value.into()),
                Candidate::Item(value.into(), "description".into()),
            ] {
                let formatted = format_candidates_for_token(vec![candidate], None);
                let output = formatted[0].output().to_string();
                assert_eq!(formatted[0].get_display_name(), value);
                assert_eq!(normalize_path_token(&output), value);
                outputs.push(output);
            }
            assert_eq!(outputs[0], outputs[1], "{value:?}");
            if [
                "hello world",
                "a;b",
                "*.rs",
                "[ab]",
                "x{a,b}",
                "#tag",
                "~root",
                "",
            ]
            .contains(&value)
            {
                assert_ne!(outputs[0], value);
            }
        }
    }

    #[test]
    fn shell_path_token_decodes_escaped_spaces() {
        let token = ShellPathToken::from_raw(r#"/tmp/dir\ with\ space/"#);
        assert_eq!(token.normalized(), "/tmp/dir with space/");
        assert_eq!(
            token.encode("/tmp/dir with space/child"),
            r#"/tmp/dir\ with\ space/child"#
        );
    }

    #[test]
    fn shell_path_token_decodes_unclosed_quotes() {
        let token = ShellPathToken::from_raw(r#""/tmp/dir with space/"#);
        assert_eq!(token.normalized(), "/tmp/dir with space/");
        assert_eq!(
            token.encode("/tmp/dir with space/child"),
            r#""/tmp/dir with space/child"#
        );
    }

    #[test]
    fn shell_path_token_preserves_single_quote_style() {
        let token = ShellPathToken::from_raw(r#"'/tmp/dir with space/"#);
        assert_eq!(token.normalized(), "/tmp/dir with space/");
        assert_eq!(
            token.encode("/tmp/dir with space/child"),
            r#"'/tmp/dir with space/child"#
        );
    }

    #[test]
    fn normalize_path_token_decodes_supported_shell_styles() {
        assert_eq!(
            normalize_path_token(r#"/tmp/dir\ with\ space/fo"#),
            "/tmp/dir with space/fo"
        );
        assert_eq!(
            normalize_path_token(r#""/tmp/dir with space/fo"#),
            "/tmp/dir with space/fo"
        );
        assert_eq!(
            normalize_path_token(r#"'/tmp/dir with space/fo"#),
            "/tmp/dir with space/fo"
        );
        assert_eq!(
            normalize_path_token(r#""/tmp/dir with space/fo""#),
            "/tmp/dir with space/fo"
        );
    }

    #[test]
    fn format_path_for_token_encodes_single_path_with_raw_style() {
        assert_eq!(
            format_path_for_token("/tmp/dir with space/foo", r#"/tmp/dir\ with\ space/fo"#),
            r#"/tmp/dir\ with\ space/foo"#
        );
        assert_eq!(
            format_path_for_token("/tmp/dir with space/foo", r#""/tmp/dir with space/fo"#),
            r#""/tmp/dir with space/foo"#
        );
        assert_eq!(
            format_path_for_token("/tmp/dir with space/foo", r#"'/tmp/dir with space/fo"#),
            r#"'/tmp/dir with space/foo"#
        );
    }

    #[test]
    fn formatted_paths_keep_display_and_quote_style() {
        use skim::SkimItem;
        let file = Candidate::File {
            path: "/tmp/dir with space/child".into(),
            is_dir: true,
        };
        for (raw, expected) in [
            (
                r#"/tmp/dir\ with\ space/"#,
                r#"/tmp/dir\ with\ space/child"#,
            ),
            (r#""/tmp/dir with space/"#, r#""/tmp/dir with space/child"#),
            (r#"'/tmp/dir with space/"#, r#"'/tmp/dir with space/child"#),
        ] {
            let formatted = format_candidates_for_token(vec![file.clone()], Some(raw));
            assert_eq!(formatted[0].get_display_name(), file.get_display_name());
            assert_eq!(formatted[0].output(), expected);
        }
    }
}
