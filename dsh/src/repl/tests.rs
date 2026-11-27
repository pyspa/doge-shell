//! Inline tests moved out of `repl/mod.rs`: background-tick timing,
//! `DoublePressState`, `input_analysis::command_is_valid`, and
//! `analyze_input` suffix calculation.
use super::*;
use crate::environment::Environment;
use crate::shell::Shell;
use std::thread;

#[tokio::test]
async fn background_interval_ticks_even_with_busy_events() {
    let mut interval = interval_at(TokioInstant::now(), Duration::from_millis(5));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut events = futures::stream::repeat(());

    let deadline = TokioInstant::now() + Duration::from_millis(50);
    let mut ticks = 0usize;

    while ticks < 3 && TokioInstant::now() < deadline {
        tokio::select! {
            _ = interval.tick() => {
                ticks += 1;
            }
            _ = events.next() => {
                tokio::task::yield_now().await;
            }
        }
    }

    assert!(
        ticks >= 3,
        "background interval ticks were starved; observed {ticks}"
    );
}

#[test]
fn test_ctrl_c_state_single_press() {
    let mut state = DoublePressState::new(3000);

    // First press returns false
    assert!(!state.on_pressed());
    assert_eq!(state.press_count, 1);
    assert!(state.first_press_time.is_some());
}

#[test]
fn test_ctrl_c_state_double_press_within_timeout() {
    let mut state = DoublePressState::new(3000);

    // First press
    assert!(!state.on_pressed());

    // Second press after short time
    thread::sleep(std::time::Duration::from_millis(100));
    assert!(state.on_pressed());
    assert_eq!(state.press_count, 2);
}

#[test]
fn test_ctrl_c_state_double_press_after_timeout() {
    let mut state = DoublePressState::new(3000);

    // First press
    assert!(!state.on_pressed());

    // Press after more than 3 seconds (treated as new first press)
    thread::sleep(std::time::Duration::from_secs(4));
    assert!(!state.on_pressed());
    assert_eq!(state.press_count, 1);
}

#[test]
fn test_ctrl_c_state_reset() {
    let mut state = DoublePressState::new(3000);

    // First press
    assert!(!state.on_pressed());

    // Reset
    state.reset();
    assert_eq!(state.press_count, 0);
    assert!(state.first_press_time.is_none());

    // Press after reset is treated as first press
    assert!(!state.on_pressed());
    assert_eq!(state.press_count, 1);
}

#[tokio::test]
async fn command_is_valid_detects_builtin_and_alias() {
    let env = Environment::new();
    {
        let mut writer = env.write();
        writer
            .variable_state
            .alias
            .insert("ll".to_string(), "ls -al".to_string());
    }

    let mut shell = Shell::new(env.clone());
    let repl = Repl::new(&mut shell);

    assert!(
        super::input_analysis::command_is_valid(&repl, "cd"),
        "built-in command should be valid"
    );
    assert!(
        super::input_analysis::command_is_valid(&repl, "ll"),
        "alias should be valid"
    );
    assert!(
        !super::input_analysis::command_is_valid(&repl, "definitely_not_a_command_42"),
        "unknown command should not be valid"
    );

    drop(repl);
}

#[tokio::test]
async fn test_analyze_input_suffix_calculation() {
    use crate::environment::Environment;
    let environment = Environment::new();
    let mut shell = Shell::new(environment);
    let mut repl = Repl::new(&mut shell);

    // Existing file for test
    let test_file = "Cargo.toml";
    let partial = "Cargo.tom";
    let suffix = "l";

    // Case 1: Cursor at end
    let input_str = format!("ls {}", partial);
    repl.input.reset(input_str.clone());

    repl.print_input(&mut Vec::new(), false, true);
    if let Some(full) = repl.input.completion.as_deref() {
        assert_eq!(full.strip_prefix(&input_str), Some(suffix));
        assert_eq!(full, format!("ls {test_file}"));
    }

    // Tab can replace the word under the cursor. Inline suffixes are only
    // offered at the end of the input, where they can be accepted.
    let input_mid = "ls Cargo.tom -lat";
    repl.input.reset(input_mid.to_string());
    repl.input.move_to("ls Cargo.tom".len());
    repl.print_input(&mut Vec::new(), false, true);
    assert!(repl.inline_suggestion_suffix().is_none());
    assert!(repl.input.color_ranges.is_some());
}

#[tokio::test]
async fn quote_closer_preserves_input_and_advances_character_cursor() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for (prefix, input, cursor, quote) in [
        ("", "echo ''", 6, '\''),
        ("", "echo \"\"", 6, '"'),
        ("", "echo '日本語' tail", 9, '\''),
        ("echo \"first\n", "日本語\" tail", 3, '"'),
    ] {
        let environment = Environment::new();
        environment.write().set_auto_pair_enabled(true);
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.state.multiline_buffer = prefix.to_string();
        repl.input.reset(input.to_string());
        repl.input.move_to_begin();
        repl.input.move_by(cursor);
        repl.handle_key_event(&KeyEvent::new(KeyCode::Char(quote), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(repl.input.as_str(), input);
        assert_eq!(repl.input.cursor(), cursor as usize + 1);
        assert_eq!(repl.state.multiline_buffer, prefix);
    }
}

#[tokio::test]
async fn quote_closer_round_trip_and_paired_backspace_preserve_existing_editing() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for (open, close) in [('"', '"'), ('\'', '\''), ('(', ')'), ('[', ']'), ('{', '}')] {
        let environment = Environment::new();
        environment.write().set_auto_pair_enabled(true);
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.handle_key_event(&KeyEvent::new(KeyCode::Char(open), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(repl.input.as_str(), format!("{open}{close}"));
        assert_eq!(repl.input.cursor(), 1);
        repl.handle_key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(repl.input.as_str(), "");
        assert_eq!(repl.input.cursor(), 0);
        for ch in [open, '日', close, 'x'] {
            repl.handle_key_event(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(repl.input.as_str(), format!("{open}日{close}x"));
        assert_eq!(repl.input.cursor(), 4);
    }
}

#[tokio::test]
async fn quote_closer_nested_substitution_preserves_literal_input_policy() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for (prefix, marked, overtype) in [
        ("", r#"echo "$(echo 'a|"b')""#, false),
        ("", r#"echo "$(echo "a|")""#, true),
        ("echo \"$(echo ", "'日本語|' )\"", true),
        ("", r#"echo "$(echo "a|""#, false),
    ] {
        let (before, after) = marked.split_once('|').unwrap();
        let input = format!("{before}{after}");
        let quote = after.chars().next().unwrap();
        let environment = Environment::new();
        environment.write().set_auto_pair_enabled(true);
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.state.multiline_buffer = prefix.to_string();
        repl.input.reset(input.clone());
        repl.input.move_to_begin();
        repl.input.move_by(before.chars().count() as isize);
        repl.handle_key_event(&KeyEvent::new(KeyCode::Char(quote), KeyModifiers::NONE))
            .await
            .unwrap();
        let expected = if overtype {
            input
        } else {
            format!("{before}{quote}{quote}{after}")
        };
        assert_eq!(repl.input.as_str(), expected, "{marked}");
        assert_eq!(repl.input.cursor(), before.chars().count() + 1);
        assert_eq!(repl.state.multiline_buffer, prefix);
    }
}
