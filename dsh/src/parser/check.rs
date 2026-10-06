use super::{Rule, ShellParser};
use pest::Parser as _;
use std::borrow::Cow;

/// Mask only grammar-recognized comments, retaining byte offsets and newlines.
/// The lexical scan also handles unfinished editor input and nested shell bodies.
fn without_comments(input: &str) -> Cow<'_, str> {
    let Ok(pairs) = ShellParser::parse(Rule::comment_scan, input) else {
        return Cow::Borrowed(input);
    };
    pairs
        .into_iter()
        .next()
        .map(super::source_without_comments)
        .unwrap_or(Cow::Borrowed(input))
}

/// Checks if the input string is incomplete and more input is expected.
/// This happens if:
/// 1. There are unclosed quotes (' or ").
/// 2. There are unclosed delimiters ((, [, {).
/// 3. The line ends with a backslash (\).
/// 4. The line ends with an operator that expects more input (|, &&, ||).
pub fn is_incomplete_input(input: &str) -> bool {
    let uncommented = without_comments(input);
    let input = uncommented.as_ref();
    let chars = input.chars().peekable();
    let mut quote_char = None;
    let mut in_backslash = false;
    let mut braces = Vec::new();

    for c in chars {
        if in_backslash {
            in_backslash = false;
            continue;
        }

        if let Some(q) = quote_char {
            if c == '\\' && q == '"' {
                in_backslash = true;
            } else if c == q {
                quote_char = None;
            }
        } else {
            match c {
                '\\' => in_backslash = true,
                '\'' | '"' => quote_char = Some(c),
                '(' | '[' | '{' => braces.push(c),
                ')' => {
                    if let Some(last) = braces.last()
                        && *last == '('
                    {
                        braces.pop();
                    }
                }
                ']' => {
                    if let Some(last) = braces.last()
                        && *last == '['
                    {
                        braces.pop();
                    }
                }
                '}' => {
                    if let Some(last) = braces.last()
                        && *last == '{'
                    {
                        braces.pop();
                    }
                }
                _ => {}
            }
        }
    }

    // 1. Unclosed quotes
    if quote_char.is_some() {
        return true;
    }

    // 2. Unclosed braces
    if !braces.is_empty() {
        return true;
    }

    // 3. Trailing backslash (escaped newline)
    if in_backslash {
        return true;
    }

    // 4. Trailing operators
    let trimmed = input.trim_end();
    if trimmed.ends_with('|')
        || trimmed.ends_with("|:")
        || trimmed.ends_with("&&")
        || trimmed.ends_with("||")
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_do_not_request_continuation_or_hide_real_incompleteness() {
        for input in [
            "# \" ( { [ \\",
            "echo OK # | && || |:",
            "echo OK # )\necho NEXT",
            "echo \"$(echo x # \" )\n)\"",
            "echo ${X:-$(echo x # )\n)}",
            "echo ${X:-(echo x # \" )\n)}",
            "echo $((1 + $(echo 1 # )\n)))",
        ] {
            assert!(!is_incomplete_input(input), "{input}");
        }
        for input in [
            "echo | # ignored",
            "echo && # ignored",
            "echo |: # ignored",
            "echo $(echo x # )",
            "echo <(echo x # )",
            "echo 'unclosed # literal",
            "echo \"unclosed # literal",
            "echo a#b '\"",
            "echo \\#escaped \"",
        ] {
            assert!(is_incomplete_input(input), "{input}");
        }
    }

    #[test]
    fn comment_scan_matches_execution_grammar_and_preserves_offsets() {
        for input in [
            "echo 日本語 # \" ( \\",
            "echo a#b ''#c \"\"#d $X#e $(echo x)#f # tail",
            "X=#value echo $# ${#} # tail\necho NEXT",
            "echo \"$(echo x # \" )\n)\" # tail",
            "echo ${X:-#literal $(echo x # )\n)} # tail",
            "echo ${X:-(echo x # \" )\n)} # tail",
            "echo $((1 + $(echo 1 # )\n))) # tail",
            "echo <(echo x # )\n) >(echo y # )\n) # tail",
            "echo x |: (list \"#t\" #t) # tail",
            "echo x |: where name == \"#name\" # tail",
        ] {
            let spans = |rule| {
                ShellParser::parse(rule, input)
                    .unwrap()
                    .flatten()
                    .filter(|pair| pair.as_rule() == Rule::comment)
                    .map(|pair| (pair.as_span().start(), pair.as_span().end()))
                    .collect::<Vec<_>>()
            };
            let execution_spans = spans(Rule::commands);
            assert_eq!(execution_spans, spans(Rule::comment_scan), "{input}");
            let masked = without_comments(input);
            assert_eq!(masked.len(), input.len());
            for (index, byte) in input.bytes().enumerate() {
                if execution_spans
                    .iter()
                    .any(|(start, end)| (*start..*end).contains(&index))
                {
                    assert_eq!(masked.as_bytes()[index], b' ');
                } else {
                    assert_eq!(masked.as_bytes()[index], byte);
                }
            }
        }
    }

    #[test]
    fn test_quotes() {
        assert!(is_incomplete_input("'hello"));
        assert!(is_incomplete_input("\"hello"));
        assert!(!is_incomplete_input("'hello'"));
        assert!(!is_incomplete_input("\"hello\""));
        assert!(is_incomplete_input("\"hello\\\"")); // escaped quote inside
    }

    #[test]
    fn test_braces() {
        assert!(is_incomplete_input("(hello"));
        assert!(!is_incomplete_input("(hello)"));
        assert!(is_incomplete_input("{hello"));
        assert!(!is_incomplete_input("{hello}"));
        assert!(is_incomplete_input("[hello"));
        assert!(!is_incomplete_input("[hello]"));
        assert!(is_incomplete_input("({["));
        assert!(!is_incomplete_input("({[]})"));
    }

    #[test]
    fn test_backslash() {
        assert!(is_incomplete_input("hello \\"));
        assert!(!is_incomplete_input("hello \\ world"));
        assert!(!is_incomplete_input("hello \\\\"));
    }

    #[test]
    fn test_operators() {
        assert!(is_incomplete_input("hello |"));
        assert!(is_incomplete_input("hello &&"));
        assert!(is_incomplete_input("hello ||"));
        assert!(!is_incomplete_input("hello | world"));
    }

    #[test]
    fn test_struct_pipe_operator() {
        // `cmd |:` alone ends in `:`, not `|`, so it needs its own check.
        assert!(is_incomplete_input("ps aux |:"));
        assert!(!is_incomplete_input("ps aux |: where cpu > 5"));
        // A trailing bare `|` inside a `|:` DSL is still a continuation, the
        // same as an ordinary trailing pipe.
        assert!(is_incomplete_input("ps aux |: where cpu > 5 |"));
    }
}
