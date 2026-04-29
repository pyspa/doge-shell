use crate::completion::display::Candidate;

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
        let normalized = match &style {
            ShellPathStyle::Plain | ShellPathStyle::Escaped => decode_backslashes(raw),
            ShellPathStyle::SingleQuoted { closed } => {
                decode_backslashes(strip_wrapping_quote(raw, '\'', *closed))
            }
            ShellPathStyle::DoubleQuoted { closed } => {
                decode_backslashes(strip_wrapping_quote(raw, '"', *closed))
            }
        };

        Self { normalized, style }
    }

    #[cfg(test)]
    fn normalized(&self) -> &str {
        &self.normalized
    }

    pub fn encode(&self, path: &str) -> String {
        match self.style {
            ShellPathStyle::Plain | ShellPathStyle::Escaped => escape_unquoted(path),
            ShellPathStyle::SingleQuoted { closed } => {
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
    ShellPathToken::from_raw(raw_token).encode(path)
}

pub fn format_candidates_for_token(
    candidates: Vec<Candidate>,
    raw_token: Option<&str>,
) -> Vec<Candidate> {
    let Some(raw_token) = raw_token.filter(|token| !token.is_empty()) else {
        return candidates;
    };
    let token = ShellPathToken::from_raw(raw_token);

    candidates
        .into_iter()
        .map(|candidate| match candidate {
            Candidate::Path(path) => Candidate::Path(token.encode(&path)),
            Candidate::File { path, is_dir } => Candidate::File {
                path: token.encode(&path),
                is_dir,
            },
            Candidate::Item(text, description)
                if description == "(file)" || description == "(directory)" =>
            {
                Candidate::Item(token.encode(&text), description)
            }
            other => other,
        })
        .collect()
}

fn detect_style(raw: &str) -> ShellPathStyle {
    if raw.starts_with('"') {
        return ShellPathStyle::DoubleQuoted {
            closed: raw.len() > 1 && raw.ends_with('"'),
        };
    }
    if raw.starts_with('\'') {
        return ShellPathStyle::SingleQuoted {
            closed: raw.len() > 1 && raw.ends_with('\''),
        };
    }
    if raw.contains('\\') {
        return ShellPathStyle::Escaped;
    }
    ShellPathStyle::Plain
}

fn strip_wrapping_quote(raw: &str, quote: char, closed: bool) -> &str {
    let inner = raw.strip_prefix(quote).unwrap_or(raw);
    if closed {
        inner.strip_suffix(quote).unwrap_or(inner)
    } else {
        inner
    }
}

fn decode_backslashes(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            } else {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
    }

    out
}

fn escape_unquoted(path: &str) -> String {
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
        if matches!(ch, '\\' | '\'') {
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
    fn format_candidates_for_token_only_rewrites_path_candidates() {
        let items = vec![
            Candidate::File {
                path: "/tmp/dir with space/child".to_string(),
                is_dir: false,
            },
            Candidate::Command {
                name: "git".to_string(),
                description: String::new(),
            },
        ];

        let formatted = format_candidates_for_token(items, Some(r#"/tmp/dir\ with\ space/"#));
        assert_eq!(
            formatted[0],
            Candidate::File {
                path: r#"/tmp/dir\ with\ space/child"#.to_string(),
                is_dir: false,
            }
        );
        assert_eq!(
            formatted[1],
            Candidate::Command {
                name: "git".to_string(),
                description: String::new(),
            }
        );
    }

    #[test]
    fn format_candidates_for_token_preserves_quote_style() {
        let items = vec![Candidate::File {
            path: "/tmp/dir with space/child".to_string(),
            is_dir: true,
        }];

        let double = format_candidates_for_token(items.clone(), Some(r#""/tmp/dir with space/"#));
        assert_eq!(
            double[0],
            Candidate::File {
                path: r#""/tmp/dir with space/child"#.to_string(),
                is_dir: true,
            }
        );

        let single = format_candidates_for_token(items, Some(r#"'/tmp/dir with space/"#));
        assert_eq!(
            single[0],
            Candidate::File {
                path: r#"'/tmp/dir with space/child"#.to_string(),
                is_dir: true,
            }
        );
    }
}
