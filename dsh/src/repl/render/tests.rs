use super::{
    preprompt_rows, print_above_prompt, print_input, print_prompt, redraw_prompt,
    render_transient_prompt_to,
};
use crate::environment::Environment;
use crate::input::{Input, InputConfig};
use crate::repl::Repl;
use crate::shell::Shell;

#[tokio::test]
async fn inline_history_skips_exact_entries_and_restores_the_suffix_at_line_end() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut shell = Shell::new(Environment::new_synthetic());
    let mut repl = Repl::new(&mut shell);
    let line = "echo 日本語";
    let extension = "echo 日本語 --verbose";
    let mut history = crate::history::History::new();
    history
        .write_batch(vec![(extension.into(), 1), (line.into(), 2)])
        .unwrap();
    repl.shell.cmd_history = Some(std::sync::Arc::new(parking_lot::Mutex::new(history)));
    repl.input.reset(line.into());
    // The input ends exactly at the margin. Its history suffix wraps once.
    repl.terminal_ui.columns = repl.terminal_ui.prompt_mark_width + repl.input.display_width();
    let mut out = Vec::new();
    repl.print_input(&mut out, false, true);
    let frame = String::from_utf8(out).unwrap();
    assert!(frame[frame.find("--verbose").unwrap()..].contains("\x1b[1A"));
    assert_eq!(repl.input.completion.as_deref(), Some(extension));

    let previous = repl.input.cursor();
    let pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.input.move_to(5);
    let mut out = Vec::new();
    repl.move_cursor_relative(&mut out, pos, previous);
    let screen = String::from_utf8(out).unwrap();
    assert!(screen.contains("\x1b[J") && !screen.contains("--verbose"));
    assert!(repl.input.completion.is_none());
    // An asynchronous redraw while editing must not bring the old ghost back.
    repl.print_input(&mut Vec::new(), false, true);
    assert!(repl.input.completion.is_none());
    assert!(repl.ai_ui.suggestion_manager.active.is_none());

    let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    repl.handle_key_event(&ctrl_e).await.unwrap();
    assert_eq!(repl.input.as_str(), line);
    assert_eq!(repl.input.cursor(), line.chars().count());
    assert_eq!(repl.input.completion.as_deref(), Some(extension));
    repl.handle_key_event(&ctrl_e).await.unwrap();
    assert_eq!(repl.input.as_str(), extension);
    assert!(repl.input.completion.is_none());
}

#[tokio::test]
async fn inline_acceptance_rejects_a_whole_line_suffix_at_a_middle_cursor() {
    use crate::repl::state::SuggestionAcceptMode;
    use crate::suggestion::{SuggestionSource, SuggestionState};
    let mut shell = Shell::new(Environment::new_synthetic());
    let mut repl = Repl::new(&mut shell);
    repl.input.reset("echo 界".into());
    repl.input.move_to(2);
    repl.ai_ui.suggestion_manager.active = Some(SuggestionState {
        full: "echo 界 --verbose".into(),
        source: SuggestionSource::History,
    });
    assert!(!repl.accept_suggestion(SuggestionAcceptMode::Word));
    repl.input.completion = Some("echo 界 --verbose".into());
    crate::repl::key_handlers::completion::handle_accept_completion(&mut repl);
    assert_eq!(repl.input.as_str(), "echo 界");
    assert_eq!(repl.input.cursor(), 2);
}

#[tokio::test]
async fn cursor_navigation_refreshes_the_tab_expansion_hint_at_the_marker() {
    let mut shell = Shell::new(Environment::new_synthetic());
    let mut repl = Repl::new(&mut shell);
    repl.terminal_ui.columns = 100;
    repl.input.reset("echo 界 |? summarize".into());
    for (target, redraw, hint) in [(0, true, false), (1, false, false), (12, true, true)] {
        let previous = repl.input.cursor();
        let pos = repl
            .input
            .cursor_pos(100, repl.terminal_ui.prompt_mark_width);
        repl.input.move_to(target);
        let mut out = Vec::new();
        repl.move_cursor_relative(&mut out, pos, previous);
        let screen = String::from_utf8(out).unwrap();
        assert_eq!(screen.contains("\x1b[J"), redraw, "{screen:?}");
        assert_eq!(screen.contains("Tab to expand"), hint, "{screen:?}");
    }
}

#[tokio::test]
async fn refreshed_analysis_clears_stale_completion_before_accepting() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.input.reset("echo audit-unique-123".into());
    for cached in [false, true] {
        repl.input.completion = Some("echo".into());
        let mut suffix = None;
        if cached {
            super::apply_cached_analysis(&mut repl, &mut suffix, None, None);
        } else {
            super::apply_fresh_analysis(
                &mut repl,
                &mut suffix,
                super::InputAnalysis {
                    completion_full: None,
                    completion: None,
                    color_ranges: None,
                    can_execute: true,
                },
            );
        }
        assert!(repl.input.completion.is_none());
        crate::repl::key_handlers::completion::handle_accept_completion(&mut repl);
        assert_eq!(repl.input.as_str(), "echo audit-unique-123");
    }
}

#[tokio::test]
async fn executable_completion_extends_the_command_at_the_end_of_the_line() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let environment = Environment::new_synthetic();
    let mut names = [
        "audit-danger;echo",
        "audit-esc\u{1b}ape",
        "audit-space name",
        "auditgrep",
        "auditgrepper",
    ]
    .map(str::to_string);
    names.sort();
    environment.write().set_executable_names(names.into());
    let mut shell = Shell::new(environment);
    let mut repl = Repl::new(&mut shell);
    let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);

    for (line, suffix) in [
        ("auditg", "rep"),
        ("  auditg", "rep"),
        ("cat a && auditg", "rep"),
        ("cat a; auditg", "rep"),
        ("cat 日本語 || auditg", "rep"),
        ("cat 日本語 | auditg", "rep"),
        ("cat a & auditg", "rep"),
        ("cat a\nauditg", "rep"),
        ("cat a | audi\\\ntg", "rep"),
        ("cat a | audit-danger", "\\;echo"),
        ("audit-space\\ na", "me"),
    ] {
        repl.input.reset(line.into());
        let mut out = Vec::new();
        repl.print_input(&mut out, false, false);
        let expected = format!("{line}{suffix}");
        assert_eq!(
            repl.input.completion.as_deref(),
            Some(expected.as_str()),
            "{line}"
        );
        let frame = String::from_utf8(out).unwrap();
        assert!(frame.contains(&format!("{suffix}\x1b[")), "{frame:?}");
        repl.handle_key_event(&ctrl_e).await.unwrap();
        assert_eq!(repl.input.as_str(), expected);
    }

    for (line, cursor) in [
        ("auditgrep", None),
        ("cat a && auditgrep", None),
        ("cat a && auditg ", None),
        ("cat auditg", None),
        ("cat a ) auditg", None),
        ("cat a && $VAR'auditg'", None),
        ("cat a && audit-esc", None),
        ("cat a && auditg", Some(5)),
    ] {
        repl.input.reset(line.into());
        if let Some(cursor) = cursor {
            repl.input.move_to(cursor);
        }
        repl.print_input(&mut Vec::new(), false, false);
        assert!(repl.input.completion.is_none(), "{line}");
        repl.handle_key_event(&ctrl_e).await.unwrap();
        assert_eq!(repl.input.as_str(), line);
        assert_eq!(repl.input.cursor(), line.chars().count());
    }
}

#[tokio::test]
async fn executable_completion_preserves_typed_spelling_and_one_literal_argv0() {
    use crate::shell::{parse::parse_execution_plan, word_expand::dry_expand_argument_word};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let environment = Environment::new_synthetic();
    let cases = [
        ("audit#compl", "ete", "audit#complete"),
        (r"audit\#compl", "ete", "audit#complete"),
        (r"audit\@compl", "ete", "audit@complete"),
        (r"audit\\", "complete", r"audit\complete"),
        ("AUDIT", r"\=complete", "AUDIT=complete"),
        ("audit-danger", r"\;\ extra", "audit-danger; extra"),
    ];
    let mut names: Vec<_> = cases
        .iter()
        .map(|(_, _, name)| (*name).to_string())
        .collect();
    names.push("~audit-exe".into());
    names.sort();
    names.dedup();
    environment.write().set_executable_names(names);
    let mut shell = Shell::new(environment.clone());
    let mut repl = Repl::new(&mut shell);
    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);

    for (word, suffix, name) in cases {
        let source = format!("echo x | {word}");
        let expected = format!("{source}{suffix}");
        repl.input.reset(source);
        repl.print_input(&mut Vec::new(), false, false);
        assert_eq!(repl.input.completion.as_deref(), Some(expected.as_str()));
        repl.handle_key_event(&right).await.unwrap();
        assert_eq!(repl.input.as_str(), expected);

        let plan = parse_execution_plan(repl.input.as_str(), environment.clone()).unwrap();
        assert_eq!(plan.lists.len(), 1);
        assert_eq!(plan.lists[0].jobs.len(), 1);
        let stages = &plan.lists[0].jobs[0].stages;
        assert_eq!(stages.len(), 2);
        assert!(stages[1].env_overrides.is_empty());
        assert_eq!(stages[1].argv.len(), 1);
        assert_eq!(
            dry_expand_argument_word(&stages[1].argv[0], &*repl.shell, std::path::Path::new("/")),
            [name]
        );
    }

    for source in [r"echo x | audit\", "echo x | ~audit"] {
        repl.input.reset(source.into());
        repl.print_input(&mut Vec::new(), false, true);
        assert!(repl.input.completion.is_none(), "{source}");
        repl.handle_key_event(&right).await.unwrap();
        assert_eq!(repl.input.as_str(), source);
    }
}

#[tokio::test]
async fn executable_completion_uses_the_logical_continuation_context() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let environment = Environment::new_synthetic();
    environment
        .write()
        .set_executable_names(vec!["audit-complete command".into()]);
    let mut shell = Shell::new(environment);
    let mut repl = Repl::new(&mut shell);
    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    let line = "audit-compl";

    for (prefix, expected) in [
        ("echo '日本語\n", line),
        ("echo 日本語 \\\n", line),
        ("echo 日本語 |\n", "audit-complete\\ command"),
    ] {
        repl.state.multiline_buffer = prefix.into();
        repl.input.reset(line.into());
        let mut out = Vec::new();
        repl.print_input(&mut out, true, false);
        repl.print_input(&mut out, false, true);
        let frame = String::from_utf8(out).unwrap();
        assert_eq!(
            frame.contains("ete\\ command"),
            expected != line,
            "{frame:?}"
        );
        repl.handle_key_event(&right).await.unwrap();
        assert_eq!(repl.input.as_str(), expected);
        assert_eq!(repl.state.multiline_buffer, prefix);

        // A refresh can also supply the suggestion while input analysis is
        // cached without a candidate, such as when the PATH scan settles.
        repl.input.reset(line.into());
        repl.ai_ui.suggestion_manager.clear();
        repl.refresh_inline_suggestion();
        assert!(repl.input.completion.is_none());
        assert_eq!(
            repl.ai_ui
                .suggestion_manager
                .active
                .as_ref()
                .map(|suggestion| suggestion.full.as_str()),
            (expected != line).then_some(expected)
        );
        repl.handle_key_event(&right).await.unwrap();
        assert_eq!(repl.input.as_str(), expected);
        assert_eq!(repl.state.multiline_buffer, prefix);
    }
}

#[tokio::test]
async fn accepting_stale_completion_preserves_current_input() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.input.reset("echo audit-unique-123".into());
    repl.input.completion = Some("echo".into());
    crate::repl::key_handlers::completion::handle_accept_completion(&mut repl);
    assert_eq!(repl.input.as_str(), "echo audit-unique-123");
}

#[tokio::test]
async fn history_completion_remains_acceptable_after_fresh_and_cached_analysis() {
    let mut shell = Shell::new(Environment::new());
    let mut repl = Repl::new(&mut shell);
    repl.input.reset("echo audit".into());
    let suffix = "-history".to_string();
    let analysis = repl.analyze_input("echo audit", Some(suffix.clone()));
    let mut completion = Some(suffix);
    super::apply_fresh_analysis(&mut repl, &mut completion, analysis);
    assert_eq!(repl.input.completion.as_deref(), Some("echo audit-history"));
    let cached = repl.ai_ui.last_analysis_result.as_ref().unwrap();
    let (full, suffix) = (cached.completion_full.clone(), cached.completion.clone());
    // Whether history is temporarily locked or reports another candidate,
    // the cached displayed suffix and the accepted full value must agree.
    for fresh_suffix in [None, Some("-different".into())] {
        repl.input.reset("echo audit".into());
        completion = fresh_suffix;
        super::apply_cached_analysis(&mut repl, &mut completion, full.clone(), suffix.clone());
        assert_eq!(completion.as_deref(), Some("-history"));
        crate::repl::key_handlers::completion::handle_accept_completion(&mut repl);
        assert_eq!(repl.input.as_str(), "echo audit-history");
    }
}

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
