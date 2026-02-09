use super::*;

#[test]
fn test_replace_range() {
    let input = "hello world";
    let result = replace_range(input, 0, 5, "hi");
    assert_eq!(result, "hi world");

    let result = replace_range(input, 6, 11, "universe");
    assert_eq!(result, "hello universe");
}

#[test]
fn test_trailing_symbol() {
    assert_eq!(trailing_symbol("ls -l"), "-l");
    assert_eq!(trailing_symbol("echo hello"), "hello");
    assert_eq!(trailing_symbol("mcp-add"), "mcp-add");
    assert_eq!(trailing_symbol("(mcp-add"), "mcp-add");
    assert_eq!(trailing_symbol(""), "");
    assert_eq!(trailing_symbol("  "), "");
}

#[test]
fn test_mcp_form_completion() {
    // Exact match
    assert_eq!(
        mcp_form_completion("mcp-add-stdio"),
        None // Already complete
    );

    // Prefix match
    assert_eq!(
        mcp_form_completion("mcp-add-s"),
        Some("mcp-add-stdio".to_string()) // First match in list
    );

    // Partial match
    assert_eq!(
        mcp_form_completion("(mcp-cle"),
        Some("(mcp-clear".to_string())
    );

    // No match
    assert_eq!(mcp_form_completion("mcp-xyz"), None);
    assert_eq!(mcp_form_completion("not-mcp"), None);
}

#[test]
fn test_next_word_chunk() {
    // Normal word
    assert_eq!(next_word_chunk("hello world"), Some("hello ".to_string()));

    // With whitespace
    assert_eq!(next_word_chunk("  hello"), Some("  hello".to_string()));

    // Trailing whitespace included if it's part of the first "chunk" logic?
    // Implementation: loop chars. if whitespace: if in_word break.
    // So "  hello world" ->
    // ' ' -> !in_word
    // ' ' -> !in_word
    // 'h' -> in_word=true
    // ...
    // 'o' -> in_word=true
    // ' ' -> in_word=true -> break.
    // So it grabs leading whitespace + word.

    assert_eq!(
        next_word_chunk("  hello world"),
        Some("  hello ".to_string())
    );

    // Single word
    assert_eq!(next_word_chunk("hello"), Some("hello".to_string()));

    // Empty
    assert_eq!(next_word_chunk(""), None);
}
