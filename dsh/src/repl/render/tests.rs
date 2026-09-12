use super::{
    preprompt_rows, print_above_prompt, print_input, print_prompt, redraw_prompt,
    render_transient_prompt_to,
};
use crate::environment::Environment;
use crate::input::{Input, InputConfig};
use crate::repl::Repl;
use crate::shell::Shell;

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn test_input(text: &str) -> Input {
    let mut input = Input::new(InputConfig::default());
    input.reset(text.to_string());
    input
}

#[tokio::test]
async fn print_prompt_resets_previous_input_redraw_height() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.last_drawn_cursor_y = 3;

    let mut output = Vec::new();
    print_prompt(&mut repl, &mut output);

    assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 0);
}

#[tokio::test]
async fn continuation_prompt_resets_previous_input_redraw_height() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.state.multiline_buffer = "echo one\n".to_string();
    repl.terminal_ui.last_drawn_cursor_y = 2;

    let mut output = Vec::new();
    print_prompt(&mut repl, &mut output);

    assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 0);
    assert_eq!(output, b"..> ");
}

#[tokio::test]
async fn print_input_after_prompt_does_not_clear_using_stale_height() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 20;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.terminal_ui.last_drawn_cursor_y = 3;

    let mut prompt_output = Vec::new();
    print_prompt(&mut repl, &mut prompt_output);

    repl.input.reset("x".to_string());
    let mut input_output = Vec::new();
    print_input(&mut repl, &mut input_output, true, false);

    assert!(!contains_bytes(&input_output, b"\x1b[3A"));
    assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 0);
}

#[tokio::test]
async fn print_input_still_tracks_current_multiline_height() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 8;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.input.reset("abcdefg".to_string());

    let mut output = Vec::new();
    print_input(&mut repl, &mut output, true, false);

    assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 1);
}

#[test]
fn preprompt_rows_counts_wrapped_lines() {
    assert_eq!(preprompt_rows("abc", 40), 1);
    // Exactly the terminal width still occupies one row: the wrap is
    // deferred until the next character.
    assert_eq!(preprompt_rows(&"a".repeat(40), 40), 1);
    assert_eq!(preprompt_rows(&"a".repeat(41), 40), 2);
    assert_eq!(preprompt_rows(&"a".repeat(81), 40), 3);
}

#[test]
fn preprompt_rows_counts_explicit_newlines() {
    assert_eq!(preprompt_rows("one\ntwo", 40), 2);
    // A wrapped segment plus a short one.
    assert_eq!(preprompt_rows(&format!("{}\nshort", "a".repeat(41)), 40), 3);
}

#[test]
fn preprompt_rows_handles_unknown_width() {
    assert_eq!(preprompt_rows("anything", 0), 1);
}

#[tokio::test]
async fn print_above_prompt_moves_past_a_wrapped_preprompt() {
    // Regression: assuming the preprompt is one row leaves an orphaned
    // fragment on screen whenever the path is wider than the terminal.
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 20;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.input.reset("abc".to_string());
    repl.terminal_ui.last_drawn_cursor_y = 0;
    // 45 columns of preprompt at 20 wide = 3 rows.
    repl.terminal_ui.last_preprompt_plain = Some("p".repeat(45));
    assert_eq!(repl.preprompt_rows(), 3);

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["[1]+  Done  x".to_string()]);

    // Must move up over all three, not the single row the old code assumed.
    assert!(contains_bytes(&output, b"\x1b[3A"));
    assert!(!contains_bytes(&output, b"\x1b[1A"));
}

#[tokio::test]
async fn print_above_prompt_does_not_emit_prompt_start_or_run_hooks() {
    // A redraw is not a new prompt: OSC 133 A here would open a command
    // block with no matching OSC 133 D.
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["done".to_string()]);

    assert!(!contains_bytes(&output, b"\x1b]133;A"));
    assert!(!contains_bytes(&output, b"\x1b]7;file://"));
}

#[tokio::test]
async fn print_prompt_emits_one_boundary_pair_but_redraws_emit_neither() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;

    let mut fresh = Vec::new();
    print_prompt(&mut repl, &mut fresh);
    assert!(contains_bytes(&fresh, b"\x1b]133;A"));
    assert_eq!(
        fresh
            .windows(b"\x1b]133;B".len())
            .filter(|window| *window == b"\x1b]133;B")
            .count(),
        1
    );

    let mut again = Vec::new();
    redraw_prompt(&mut repl, &mut again);
    assert!(!contains_bytes(&again, b"\x1b]133;A"));
    assert!(!contains_bytes(&again, b"\x1b]133;B"));

    repl.input.reset("echo one".to_string());
    let mut input_redraws = Vec::new();
    print_input(&mut repl, &mut input_redraws, true, false);
    print_input(&mut repl, &mut input_redraws, false, false);
    assert!(!contains_bytes(&input_redraws, b"\x1b]133;B"));
}

#[tokio::test]
async fn print_prompt_records_the_preprompt_for_row_counting() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;

    let mut output = Vec::new();
    print_prompt(&mut repl, &mut output);

    // Recorded ANSI-stripped so the row count reflects display width.
    let plain = repl.terminal_ui.last_preprompt_plain.as_deref().unwrap();
    assert!(!plain.contains('\x1b'));
    assert!(repl.preprompt_rows() >= 1);
}

#[tokio::test]
async fn continuation_prompt_reports_no_preprompt_rows() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;
    repl.state.multiline_buffer = "echo one\n".to_string();

    let mut output = Vec::new();
    print_prompt(&mut repl, &mut output);

    assert!(repl.terminal_ui.last_preprompt_plain.is_none());
    assert_eq!(repl.preprompt_rows(), 0);
}

#[tokio::test]
async fn print_above_prompt_moves_past_preprompt_and_clears() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.input.reset("abc".to_string());
    repl.terminal_ui.last_drawn_cursor_y = 0;
    repl.terminal_ui.last_preprompt_plain = Some("~/repo".to_string());

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["[1]+  Done  sleep 1".to_string()]);

    // One row up for the preprompt line, then clear everything below.
    assert!(contains_bytes(&output, b"\x1b[1A"));
    assert!(contains_bytes(&output, b"\x1b[J"));
    assert!(contains_bytes(&output, b"[1]+  Done  sleep 1"));
}

#[tokio::test]
async fn print_above_prompt_in_continuation_mode_skips_preprompt_line() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;
    repl.terminal_ui.prompt_mark_cache = "..> ".to_string();
    repl.terminal_ui.prompt_mark_width = 4;
    repl.state.multiline_buffer = "echo one\n".to_string();
    repl.terminal_ui.last_drawn_cursor_y = 0;

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["[1]+  Done  x".to_string()]);

    // No preprompt line exists in continuation mode, so nothing to move past.
    assert!(!contains_bytes(&output, b"\x1b[1A"));
    assert!(contains_bytes(&output, b"[1]+  Done  x"));
}

#[tokio::test]
async fn print_above_prompt_multiline_input_moves_up_cursor_row_plus_one() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 8;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.input.reset("abcdefg".to_string());
    // "> abcdefg" wraps at 8 columns, so the cursor sits on row 1.
    repl.terminal_ui.last_drawn_cursor_y = 1;
    repl.terminal_ui.last_preprompt_plain = Some("~".to_string());

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["done".to_string()]);

    // 1 input row + 1 preprompt row.
    assert!(contains_bytes(&output, b"\x1b[2A"));
}

#[tokio::test]
async fn print_above_prompt_noop_when_columns_zero() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 0;

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["done".to_string()]);

    assert!(output.is_empty());
}

#[tokio::test]
async fn print_above_prompt_noop_when_no_lines() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &[]);

    assert!(output.is_empty());
}

#[tokio::test]
async fn print_above_prompt_preserves_input_buffer() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.input.reset("git comm".to_string());

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["done".to_string()]);

    assert_eq!(repl.input.as_str(), "git comm");
    // The input is redrawn below the notice. Syntax highlighting splits it
    // into colored runs, so assert on the individual tokens.
    assert!(contains_bytes(&output, b"done"));
    assert!(contains_bytes(&output, b"git"));
    assert!(contains_bytes(&output, b"comm"));
}

#[tokio::test]
async fn print_above_prompt_clears_last_explanation() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 40;
    repl.terminal_ui.prompt_mark_cache = "> ".to_string();
    repl.terminal_ui.prompt_mark_width = 2;
    repl.ai_ui.last_explanation = Some("stale hint".to_string());

    let mut output = Vec::new();
    print_above_prompt(&mut repl, &mut output, &["done".to_string()]);

    assert!(repl.ai_ui.last_explanation.is_none());
}

#[test]
fn transient_prompt_does_not_overcount_exact_terminal_edge() {
    let input = test_input("abc");
    let mut output = Vec::new();

    render_transient_prompt_to(&mut output, &input, 2, 5).expect("render transient prompt");

    assert!(contains_bytes(&output, b"\x1b[1A"));
    assert!(!contains_bytes(&output, b"\x1b[2A"));
}

#[test]
fn transient_prompt_uses_current_cursor_line_not_full_input_height() {
    let mut input = test_input("abcdefghijklmnop");
    input.move_to_begin();
    input.move_by(9);
    let mut output = Vec::new();

    render_transient_prompt_to(&mut output, &input, 2, 8).expect("render transient prompt");

    assert!(contains_bytes(&output, b"\x1b[2A"));
    assert!(!contains_bytes(&output, b"\x1b[3A"));
}

#[test]
fn transient_prompt_skips_clear_when_terminal_width_is_unknown() {
    let input = test_input("ls -al");
    let mut output = Vec::new();

    render_transient_prompt_to(&mut output, &input, 2, 0).expect("render transient prompt");

    assert!(output.is_empty());
}
