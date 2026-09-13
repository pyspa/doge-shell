use super::*;
use crate::environment::Environment;
use crate::input::{Input, InputConfig};
use std::fs;

fn completion_input(input: &str) -> Input {
    let mut state = Input::new(InputConfig::default());
    state.reset(input.to_string());
    state
}

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

#[test]
fn test_argument_path_completion_preserves_escaped_style() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    fs::create_dir(&spaced).unwrap();
    fs::File::create(spaced.join("foo.txt")).unwrap();

    let raw_prefix = format!("{}/dir\\ with\\ space/fo", dir.path().display());
    let input = format!("ls {raw_prefix}");
    let expected = format!("ls {}/dir\\ with\\ space/foo.txt", dir.path().display());

    let completion = complete_path_for_span(&input, 3, input.len(), false).unwrap();

    assert_eq!(completion.full, expected);
    assert_eq!(completion.suffix, "o.txt");
}

#[test]
fn test_argument_path_completion_replaces_quoted_raw_token() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    fs::create_dir(&spaced).unwrap();
    fs::File::create(spaced.join("foo.txt")).unwrap();

    let input = format!("ls \"{}/dir with space/fo", dir.path().display());
    let expected = format!("ls \"{}/dir with space/foo.txt", dir.path().display());

    let completion = complete_path_for_span(&input, 4, input.len(), false).unwrap();

    assert_eq!(completion.full, expected);
    assert_eq!(completion.suffix, "o.txt");
}

#[test]
fn test_argument_path_completion_directory_only() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    fs::create_dir(&spaced).unwrap();
    fs::File::create(spaced.join("foo.txt")).unwrap();
    fs::create_dir(spaced.join("foodir")).unwrap();

    let raw_prefix = format!("{}/dir\\ with\\ space/fo", dir.path().display());
    let input = format!("cd {raw_prefix}");
    let expected = format!("cd {}/dir\\ with\\ space/foodir/", dir.path().display());

    let completion = complete_path_for_span(&input, 3, input.len(), true).unwrap();

    assert_eq!(completion.full, expected);
    assert_eq!(completion.suffix, "odir/");
}

#[test]
fn test_command_path_completion_preserves_escaped_style() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    fs::create_dir(&spaced).unwrap();
    fs::create_dir(spaced.join("foodir")).unwrap();

    let input = format!("{}/dir\\ with\\ space/fo", dir.path().display());
    let expected = format!("{}/dir\\ with\\ space/foodir/", dir.path().display());
    let input_state = completion_input(&input);
    let environment = Environment::new();

    let suggestion = completion_suggestion(&input_state, &input, &environment).unwrap();

    assert_eq!(suggestion.full, expected);
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}

#[test]
fn test_command_path_completion_replaces_quoted_raw_token() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    fs::create_dir(&spaced).unwrap();
    fs::create_dir(spaced.join("foodir")).unwrap();

    let input = format!("\"{}/dir with space/fo", dir.path().display());
    let expected = format!("\"{}/dir with space/foodir/", dir.path().display());

    let completion = complete_path_for_span(&input, 1, input.len(), true).unwrap();

    assert_eq!(completion.full, expected);
    assert_eq!(completion.suffix, "odir/");
}

#[test]
fn test_command_path_completion_keeps_directory_only_policy() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    fs::create_dir(&spaced).unwrap();
    fs::File::create(spaced.join("foo.txt")).unwrap();

    let input = format!("{}/dir\\ with\\ space/fo", dir.path().display());
    let input_state = completion_input(&input);
    let environment = Environment::new();

    assert_eq!(
        completion_suggestion(&input_state, &input, &environment),
        None
    );
}

#[test]
fn test_command_prefix_completion_still_uses_environment_first() {
    let input = "zz-test";
    let input_state = completion_input(input);
    let environment = Environment::new();
    environment
        .write()
        .set_executable_names(vec!["zz-test-command".to_string()]);

    let suggestion = completion_suggestion(&input_state, input, &environment).unwrap();

    assert_eq!(suggestion.full, "zz-test-command");
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}
