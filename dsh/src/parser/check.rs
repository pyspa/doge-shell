/// Checks if the input string is incomplete and more input is expected.
/// This happens if:
/// 1. There are unclosed quotes (' or ").
/// 2. There are unclosed delimiters ((, [, {).
/// 3. The line ends with a backslash (\).
/// 4. The line ends with an operator that expects more input (|, &&, ||).
pub fn is_incomplete_input(input: &str) -> bool {
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
    // Remove comments first? (simplification: assume incomplete logic doesn't strictly parse comments yet,
    // but typical shell behavior treats # as comment start.
    // However, if we are inside a string, we handled it. Outside string, # starts comment.)
    // We should probably strip comments from the end before checking trailing operators.

    // A simplified check for trailing operators on the *original* input might be risky if they are in comments.
    // Let's rely on the tokenizer state we just ran?
    // Actually, let's just do a quick backward scan ignoring whitespace and comments.

    // Complex implementation for operators might need a more robust tokenizer or reuse the logic above.
    // For now, let's stick to the quote/brace/backslash check as primary drivers for multiline.
    // Operators | && || usually just fail in strict parse, checking them for "continuation" is a nice to have.
    // Let's implement trailing operator check carefully.

    let trimmed = input.trim_end();
    if trimmed.ends_with('|') || trimmed.ends_with("&&") || trimmed.ends_with("||") {
        // Need to verify these are not inside comments or strings.
        // Since we already walked the string, we know if we ended in a quote.
        // But we didn't track "valid code" vs "comment".
        // Let's just return false for now for operators to avoid complexity,
        // OR we can rely on pest failure? No, pest failure doesn't verify "incomplete" vs "error".

        // Let's refine the loop to track "last significant token".
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
