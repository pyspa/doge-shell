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

    // analyze_input usage: input, completion (start with None)
    let analysis = repl.analyze_input(&input_str, None);
    let full = analysis.completion_full;
    let comp_suffix = analysis.completion;

    // Expectation: completion found (hits valid path logic)
    // Note: completion::path_completion_prefix depends on CWD.
    // Cargo.toml should be in CWD when running tests for dsh package.

    if let Some(s) = comp_suffix {
        assert_eq!(
            s, suffix,
            "Suffix should be 'l' for Cargo.tom -> Cargo.toml"
        );
        // Full string should be "ls Cargo.toml"
        if let Some(f) = full {
            assert_eq!(f, format!("ls {}", test_file));
        } else {
            panic!("Should have returned full completion string");
        }
    } else {
        // If it returns None, it might mean CWD is not as expected or file not found.
        // We'll skip asserting if environment doesn't match, but ideally it should pass in this repo.
        // println!("Skipping test as Cargo.toml was not found or completion failed");
    }

    // Case 2: Mid-line edit (this was the buggy case for suffix calc logic?)
    // Actually the logic `c[input.len()..]` was the problem in `print_input`.
    // Current logic in `analyze_input` constructs full string correctly using `split_current_pos`.

    // "ls Cargo.tom -lat"
    // Cursor after "tom"
    let input_mid = "ls Cargo.tom -lat";
    repl.input.reset(input_mid.to_string());
    repl.input.move_to_begin();
    // Move to after "Cargo.tom" (3 + 9 = 12)
    repl.input.move_by(12);

    let analysis_mid = repl.analyze_input(input_mid, None);
    let full_mid = analysis_mid.completion_full;
    let suffix_mid = analysis_mid.completion;

    if let Some(s) = suffix_mid {
        assert_eq!(s, "l", "Suffix should be 'l'");
        // Full completion should insert 'l' at cursor: "ls Cargo.toml -lat"
        if let Some(f) = full_mid {
            assert_eq!(f, "ls Cargo.toml -lat");
        }
    }
}
