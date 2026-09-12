use super::*;

/// A buffer whose undo history starts empty, as it would at the start of a
/// fresh command line.
fn input_with(text: &str) -> Input {
    let mut input = Input::new(InputConfig::default());
    input.reset(text.to_string());
    input.undo_stack.clear();
    input.redo_stack.clear();
    input.last_edit_kind = None;
    input
}

// === kill ring ===

#[test]
fn kill_to_end_then_yank_round_trips() {
    let mut input = input_with("echo hello world");
    input.cursor = 5;
    input.delete_to_end();
    assert_eq!(input.as_str(), "echo ");
    assert_eq!(input.kill_ring(), "hello world");

    assert!(input.yank());
    assert_eq!(input.as_str(), "echo hello world");
}

#[test]
fn kill_to_beginning_then_yank_round_trips() {
    let mut input = input_with("echo hello");
    input.cursor = 5;
    input.delete_to_beginning();
    assert_eq!(input.as_str(), "hello");
    assert_eq!(input.kill_ring(), "echo ");

    input.move_to_begin();
    assert!(input.yank());
    assert_eq!(input.as_str(), "echo hello");
}

#[test]
fn kill_word_backward_saves_the_word() {
    let mut input = input_with("git commit");
    input.delete_word_backward();
    assert_eq!(input.as_str(), "git ");
    assert_eq!(input.kill_ring(), "commit");
}

#[test]
fn kill_ring_survives_multibyte_text() {
    let mut input = input_with("echo あいう");
    input.cursor = 5;
    input.delete_to_end();
    assert_eq!(input.kill_ring(), "あいう");
    assert!(input.yank());
    assert_eq!(input.as_str(), "echo あいう");
}

#[test]
fn yank_with_empty_kill_ring_is_a_noop() {
    let mut input = input_with("abc");
    assert!(!input.yank());
    assert_eq!(input.as_str(), "abc");
}

#[test]
fn yank_keeps_the_kill_ring_for_repeated_pastes() {
    let mut input = input_with("abc");
    input.delete_to_beginning();
    assert!(input.yank());
    assert!(input.yank());
    assert_eq!(input.as_str(), "abcabc");
}

// === undo / redo ===

#[test]
fn undo_restores_the_previous_buffer() {
    let mut input = input_with("abc");
    input.delete_to_beginning();
    assert_eq!(input.as_str(), "");

    assert!(input.undo());
    assert_eq!(input.as_str(), "abc");
}

#[test]
fn redo_reapplies_an_undone_edit() {
    let mut input = input_with("abc");
    input.delete_to_beginning();
    input.undo();
    assert_eq!(input.as_str(), "abc");

    assert!(input.redo());
    assert_eq!(input.as_str(), "");
}

#[test]
fn undo_with_empty_history_returns_false() {
    let mut input = input_with("abc");
    assert!(!input.undo());
    assert_eq!(input.as_str(), "abc");
}

#[test]
fn consecutive_typing_within_a_word_collapses_into_one_undo_step() {
    let mut input = input_with("");
    for ch in "hello".chars() {
        input.insert(ch);
    }
    assert_eq!(input.as_str(), "hello");

    assert!(input.undo());
    assert_eq!(input.as_str(), "");
    assert!(!input.undo());
}

#[test]
fn undo_breaks_at_word_boundaries() {
    // A single undo must not wipe the whole typed line.
    let mut input = input_with("");
    for ch in "echo keep DROP".chars() {
        input.insert(ch);
    }
    assert_eq!(input.as_str(), "echo keep DROP");

    assert!(input.undo());
    assert_eq!(input.as_str(), "echo keep ");
    assert!(input.undo());
    assert_eq!(input.as_str(), "echo keep");
    assert!(input.undo());
    assert_eq!(input.as_str(), "echo ");
}

#[test]
fn redo_walks_back_through_word_groups() {
    let mut input = input_with("");
    for ch in "echo keep DROP".chars() {
        input.insert(ch);
    }
    input.undo();
    assert_eq!(input.as_str(), "echo keep ");

    assert!(input.redo());
    assert_eq!(input.as_str(), "echo keep DROP");
}

#[test]
fn a_delete_between_inserts_starts_a_new_undo_step() {
    let mut input = input_with("");
    input.insert('a');
    input.insert('b');
    input.backspace();
    input.insert('c');
    assert_eq!(input.as_str(), "ac");

    input.undo(); // undo the "c"
    assert_eq!(input.as_str(), "a");
    input.undo(); // undo the backspace
    assert_eq!(input.as_str(), "ab");
    input.undo(); // undo the "ab" run
    assert_eq!(input.as_str(), "");
}

#[test]
fn delete_word_backward_is_a_single_undo_step() {
    let mut input = input_with("git commit");
    input.delete_word_backward();
    assert_eq!(input.as_str(), "git ");

    assert!(input.undo());
    assert_eq!(input.as_str(), "git commit");
    assert!(!input.undo());
}

#[test]
fn a_new_edit_after_undo_clears_the_redo_stack() {
    let mut input = input_with("abc");
    input.delete_to_beginning();
    input.undo();
    input.insert('z');

    assert!(!input.redo());
}

#[test]
fn clear_drops_the_undo_history() {
    let mut input = input_with("abc");
    input.delete_to_beginning();
    input.clear();

    assert!(!input.undo());
    assert!(!input.redo());
}

#[test]
fn undo_history_is_capped() {
    let mut input = input_with("");
    // Alternate kinds so every edit records its own step.
    for _ in 0..(MAX_UNDO_DEPTH + 20) {
        input.insert('a');
        input.backspace();
    }
    assert!(input.undo_stack.len() <= MAX_UNDO_DEPTH);
}

#[test]
fn undo_clamps_a_stale_cursor() {
    let mut input = input_with("abcdef");
    input.move_to_end();
    input.delete_to_beginning();
    // Cursor was 6 before the edit; the restored buffer is 6 chars long.
    assert!(input.undo());
    assert!(input.cursor() <= input.len());
}

#[test]
fn test_input_creation_and_display() {
    let config = InputConfig::default();
    let input = Input::new(config);

    assert_eq!(input.as_str(), "");
    assert_eq!(input.cursor(), 0);
    assert_eq!(format!("{input}"), "");
}

#[test]
fn test_input_operations() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    // Character input test
    input.insert('h');
    input.insert('i');
    assert_eq!(input.as_str(), "hi");
    assert_eq!(input.cursor(), 2);

    // Cursor movement test
    input.move_to_end();
    assert_eq!(input.cursor(), 2);
}

#[test]
fn history_match_color_range_uses_background_style() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.reset_with_color_ranges(
        "git status".to_string(),
        vec![(4, 10, ColorType::HistoryMatch)],
    );

    crossterm::style::force_color_output(true);
    let mut output = Vec::new();
    input.print(&mut output, None);
    crossterm::style::force_color_output(false);
    let rendered = String::from_utf8(output).unwrap();

    assert!(rendered.contains("38;5;0m"), "{rendered:?}");
    assert!(rendered.contains("48;5;11m"), "{rendered:?}");
    assert!(rendered.contains("status"));
}

#[test]
fn editing_clears_color_ranges() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.reset_with_color_ranges(
        "git status".to_string(),
        vec![(4, 10, ColorType::HistoryMatch)],
    );

    input.insert('!');

    assert!(input.color_ranges.is_none());
}

#[test]
fn test_replace_range_chars_ascii() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.insert_str("git status");

    assert!(input.replace_range_chars(4, 10, "stash"));
    assert_eq!(input.as_str(), "git stash");
    assert_eq!(input.cursor(), 9);
}

#[test]
fn test_replace_range_chars_multibyte() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.insert_str("cmd あい うえ");

    assert!(input.replace_range_chars(4, 6, "お"));
    assert_eq!(input.as_str(), "cmd お うえ");
    assert_eq!(input.cursor(), 5);
    assert_eq!(input.cursor_pos(80, 0).0, 6);
}

#[test]
fn test_replace_range_chars_rejects_invalid_range() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.insert_str("abc");

    assert!(!input.replace_range_chars(3, 1, "x"));
    assert!(!input.replace_range_chars(0, 4, "x"));
    assert_eq!(input.as_str(), "abc");
    assert_eq!(input.cursor(), 3);
}

#[test]
fn test_unicode_width_calculation() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    // Test ASCII characters (width = 1 each)
    input.insert('a');
    input.insert('b');
    assert_eq!(input.cursor(), 2);
    assert_eq!(input.cursor_pos(80, 0).0, 2);

    // Clear and test Japanese characters (width = 2 each)
    input.clear();
    input.insert('あ'); // Japanese hiragana 'a'
    input.insert('い'); // Japanese hiragana 'i'
    assert_eq!(input.cursor(), 2); // 2 characters
    assert_eq!(input.cursor_pos(80, 0).0, 4); // 4 display width

    // Test mixed ASCII and Japanese
    input.clear();
    input.insert('a'); // width = 1
    input.insert('あ'); // width = 2
    input.insert('b'); // width = 1
    assert_eq!(input.cursor(), 3); // 3 characters
    assert_eq!(input.cursor_pos(80, 0).0, 4); // 1 + 2 + 1 = 4 display width
}

#[test]
fn test_cursor_movement_with_unicode() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    // Insert mixed characters
    input.insert('a'); // width = 1
    input.insert('あ'); // width = 2
    input.insert('b'); // width = 1

    // Move cursor to different positions and check display width
    input.move_to_begin();
    assert_eq!(input.cursor(), 0);
    assert_eq!(input.cursor_pos(80, 0).0, 0);

    input.move_by(1); // After 'a'
    assert_eq!(input.cursor(), 1);
    assert_eq!(input.cursor_pos(80, 0).0, 1);

    input.move_by(1); // After 'あ'
    assert_eq!(input.cursor(), 2);
    assert_eq!(input.cursor_pos(80, 0).0, 3); // 1 + 2

    input.move_by(1); // After 'b'
    assert_eq!(input.cursor(), 3);
    assert_eq!(input.cursor_pos(80, 0).0, 4); // 1 + 2 + 1
}

#[test]
fn test_backspace_with_multibyte_characters() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    input.insert('a');
    input.insert('あ');
    input.insert('b');

    input.backspace();
    assert_eq!(input.as_str(), "aあ");
    assert_eq!(input.cursor(), 2);

    input.backspace();
    assert_eq!(input.as_str(), "a");
    assert_eq!(input.cursor(), 1);

    input.backspace();
    assert_eq!(input.as_str(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn test_delete_char_with_multibyte_characters() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    input.insert_str("ab😀c");
    input.move_to_end();
    input.move_by(-2);

    input.delete_char();
    assert_eq!(input.as_str(), "abc");
    assert_eq!(input.cursor(), 2);
}

// Moved display_width tests to utils.rs

#[test]
fn test_delete_to_end() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    // Test at beginning
    input.reset("hello".to_string());
    input.move_to_begin();
    input.delete_to_end();
    assert_eq!(input.as_str(), "");
    assert_eq!(input.cursor(), 0);

    // Test in middle
    input.reset("hello".to_string());
    input.move_to_begin();
    input.move_by(2); // "he|llo"
    input.delete_to_end();
    assert_eq!(input.as_str(), "he");
    assert_eq!(input.cursor(), 2);

    // Test at end
    input.reset("hello".to_string());
    input.move_to_end();
    input.delete_to_end();
    assert_eq!(input.as_str(), "hello");
    assert_eq!(input.cursor(), 5);

    // Test with multi-byte
    input.reset("あいうえお".to_string());
    input.move_to_begin();
    input.move_by(2); // "あい|うえお"
    input.delete_to_end();
    assert_eq!(input.as_str(), "あい");
    assert_eq!(input.cursor(), 2);
}

#[test]
fn test_delete_to_beginning() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    // Test at beginning (nothing happens)
    input.reset("hello".to_string());
    input.move_to_begin();
    input.delete_to_beginning();
    assert_eq!(input.as_str(), "hello");
    assert_eq!(input.cursor(), 0);

    // Test in middle
    input.reset("hello".to_string());
    input.move_to_begin();
    input.move_by(2); // "he|llo"
    input.delete_to_beginning();
    assert_eq!(input.as_str(), "llo");
    assert_eq!(input.cursor(), 0);

    // Test at end
    input.reset("hello".to_string());
    input.move_to_end();
    input.delete_to_beginning();
    assert_eq!(input.as_str(), "");
    assert_eq!(input.cursor(), 0);

    // Test with multi-byte
    input.reset("あいうえお".to_string());
    input.move_to_begin();
    input.move_by(2); // "あい|うえお"
    input.delete_to_beginning();
    assert_eq!(input.as_str(), "うえお");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn test_completion_word_fallback_for_redirect() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    for ch in "cat > fo".chars() {
        input.insert(ch);
    }

    let fallback = input.get_completion_word_fallback();
    assert_eq!(fallback.as_deref(), Some("fo"));
}

#[test]
fn test_completion_word_fallback_handles_whitespace_boundary() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    for ch in "echo foo".chars() {
        input.insert(ch);
    }

    let fallback = input.get_completion_word_fallback();
    assert_eq!(fallback.as_deref(), Some("foo"));

    input.insert(' ');
    let fallback_after_space = input.get_completion_word_fallback();
    assert_eq!(fallback_after_space, None);
}

#[test]
fn test_completion_word_fallback_preserves_escaped_space_token() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.reset(r#"cat dir\ with\ space/fo"#.to_string());

    let fallback = input.get_completion_word_fallback();

    assert_eq!(fallback.as_deref(), Some(r#"dir\ with\ space/fo"#));
}

#[test]
fn test_completion_word_fallback_preserves_quoted_tokens() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.reset(r#"cat "dir with space/fo"#.to_string());

    let fallback = input.get_completion_word_fallback();

    assert_eq!(fallback.as_deref(), Some(r#""dir with space/fo"#));

    input.reset(r#"cat 'dir with space/fo"#.to_string());
    let single_quoted = input.get_completion_word_fallback();

    assert_eq!(single_quoted.as_deref(), Some(r#"'dir with space/fo"#));
}

#[test]
fn test_completion_word_fallback_respects_operator_separator() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.reset("cmd | foo".to_string());

    let fallback = input.get_completion_word_fallback();

    assert_eq!(fallback.as_deref(), Some("foo"));
}

#[test]
fn test_completion_word_fallback_returns_none_on_token_start() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.reset("cmd | foo".to_string());
    input.move_to_begin();
    input.move_by(6);

    let fallback = input.get_completion_word_fallback();

    assert_eq!(fallback, None);
}

#[test]
fn test_word_navigation_and_deletion() {
    let config = InputConfig::default();
    let mut input = Input::new(config);
    input.insert_str("echo hello world");

    // Initial state: "echo hello world|"
    assert_eq!(input.cursor(), 16);

    // Test Ctrl+W (delete "world")
    input.delete_word_backward();
    assert_eq!(input.as_str(), "echo hello ");
    assert_eq!(input.cursor(), 11);

    // Test delete "hello"
    input.delete_word_backward();
    assert_eq!(input.as_str(), "echo ");
    assert_eq!(input.cursor(), 5);

    // Test delete "echo"
    input.delete_word_backward();
    assert_eq!(input.as_str(), "");
    assert_eq!(input.cursor(), 0);

    // restore
    input.insert_str("one two three");
    // "one two three|"

    // Test Move Left
    input.move_word_left(); // to start of "three"
    assert_eq!(input.cursor(), 8); // "one two |three"

    input.move_word_left(); // to start of "two"
    assert_eq!(input.cursor(), 4); // "one |two three"

    input.move_word_left(); // to start of "one"
    assert_eq!(input.cursor(), 0); // "|one two three"

    // Test Move Right
    input.move_word_right(); // to end of "one"
    assert_eq!(input.cursor(), 4); // "one| two three"

    input.move_word_right();
    assert_eq!(input.cursor(), 8); // start of "three"

    input.move_word_right();
    assert_eq!(input.cursor(), 13); // end of string
}

#[test]
fn test_set_cursor_from_display_width() {
    let config = InputConfig::default();
    let mut input = Input::new(config);

    // Test with ASCII
    input.insert_str("abcdef");

    // Target width exact matches
    input.set_cursor_from_display_width(0);
    assert_eq!(input.cursor(), 0);

    input.set_cursor_from_display_width(1);
    assert_eq!(input.cursor(), 1);

    input.set_cursor_from_display_width(3);
    assert_eq!(input.cursor(), 3);

    // Target width beyond end of string
    input.set_cursor_from_display_width(100);
    assert_eq!(input.cursor(), 6);

    // Test with multi-byte characters
    input.clear();
    input.insert_str("あいう");

    // The 'あ' is width 2.
    // Clicking at width 0 snaps to 0.
    input.set_cursor_from_display_width(0);
    assert_eq!(input.cursor(), 0);

    // Clicking at width 1 (middle of 'あ') snaps to 1 (after 'あ').
    input.set_cursor_from_display_width(1);
    assert_eq!(input.cursor(), 1);

    // Clicking at width 2 (start of 'い') snaps to 1.
    input.set_cursor_from_display_width(2);
    assert_eq!(input.cursor(), 1);

    // Clicking at width 3 (middle of 'い') snaps to 2.
    input.set_cursor_from_display_width(3);
    assert_eq!(input.cursor(), 2);

    // Test mixed characters
    input.clear();
    input.insert_str("aあbいc");

    // "a" (0..1) -> width 0-1
    // "あ" (1..2) -> width 1-3
    // "b" (2..3) -> width 3-4
    // "い" (3..4) -> width 4-6
    // "c" (4..5) -> width 6-7

    input.set_cursor_from_display_width(0);
    assert_eq!(input.cursor(), 0);

    input.set_cursor_from_display_width(1);
    assert_eq!(input.cursor(), 1);

    input.set_cursor_from_display_width(2);
    assert_eq!(input.cursor(), 2);

    input.set_cursor_from_display_width(3);
    assert_eq!(input.cursor(), 2);

    input.set_cursor_from_display_width(4);
    assert_eq!(input.cursor(), 3);

    input.set_cursor_from_display_width(10);
    assert_eq!(input.cursor(), 5);
}
