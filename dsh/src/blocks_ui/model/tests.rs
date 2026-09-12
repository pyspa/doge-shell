use super::*;
use std::time::{Duration, SystemTime};

fn block(id: u64, command: &str, exit_code: i32, stdout: &str) -> CommandBlock {
    CommandBlock {
        id,
        command: command.to_string(),
        cwd: Some("/repo".to_string()),
        stdout: stdout.to_string(),
        stderr: String::new(),
        exit_code,
        timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        duration_ms: 100,
        output_entry_ids: Vec::new(),
        watched: false,
        watch_summary: None,
    }
}

/// `get_all_blocks` order: newest first, because `push` uses `push_front`.
fn sample() -> BlockBrowser {
    BlockBrowser::new(vec![
        block(3, "git status", 0, "clean"),
        block(2, "cargo test", 1, "test failed"),
        block(1, "cargo build", 0, "compiling\ndone"),
    ])
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

fn commands(browser: &BlockBrowser) -> Vec<String> {
    browser
        .blocks()
        .into_iter()
        .map(|b| b.command.clone())
        .collect()
}

#[test]
fn mark_toggles_and_reports_count() {
    let mut b = sample();
    assert_eq!(b.marked_count(), 0);
    assert_eq!(b.on_key(key(KeyCode::Char('m'))), BrowserAction::Redraw);
    assert!(b.is_marked(0));
    assert_eq!(b.marked_count(), 1);
    assert_eq!(b.status(), Some("1 marked for export"));

    b.on_key(key(KeyCode::Char('m')));
    assert!(!b.is_marked(0));
    assert_eq!(b.marked_count(), 0);
}

#[test]
fn export_uses_marked_ids_sorted_or_falls_back_to_selection() {
    let mut b = sample();
    // Mark "git status" (id 3) and "cargo build" (id 1).
    b.on_key(key(KeyCode::Char('m')));
    b.on_key(key(KeyCode::Char('G')));
    b.on_key(key(KeyCode::Char('m')));

    let action = b.on_key(key(KeyCode::Char('x')));
    let BrowserAction::Finish(BrowserOutcome::Run(command)) = action else {
        panic!("expected export command, got {action:?}");
    };
    assert!(command.starts_with("blocks export --ids 1,3 -o runbook-"));
    assert!(command.ends_with(".md"));

    // No marks: export the selected block by its stable id.
    let mut b = sample();
    b.on_key(key(KeyCode::Char('j')));
    let action = b.on_key(key(KeyCode::Char('x')));
    let BrowserAction::Finish(BrowserOutcome::Run(command)) = action else {
        panic!("expected export command, got {action:?}");
    };
    assert!(command.starts_with("blocks export --ids 2 -o runbook-"));
}

#[test]
fn export_ids_survive_an_active_filter() {
    let mut b = sample();
    b.on_key(key(KeyCode::Char('/')));
    for ch in "cargo build".chars() {
        b.on_key(key(KeyCode::Char(ch)));
    }
    b.on_key(key(KeyCode::Enter));
    assert_eq!(commands(&b), vec!["cargo build"]);

    b.on_key(key(KeyCode::Char('m')));
    let action = b.on_key(key(KeyCode::Char('x')));
    let BrowserAction::Finish(BrowserOutcome::Run(command)) = action else {
        panic!("expected export command, got {action:?}");
    };
    // "cargo build" has stable id 1, not its filtered position.
    assert!(command.starts_with("blocks export --ids 1 -o runbook-"));
}

#[test]
fn blocks_keep_the_history_order_which_is_newest_first() {
    // Reordering here would desync the `blocks explain N` numbering.
    assert_eq!(
        commands(&sample()),
        vec!["git status", "cargo test", "cargo build"]
    );
}

#[test]
fn selection_moves_and_stops_at_the_ends() {
    let mut b = sample();
    assert_eq!(b.selected(), 0);
    assert_eq!(b.on_key(key(KeyCode::Up)), BrowserAction::Noop);

    assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Redraw);
    assert_eq!(b.selected(), 1);
    b.on_key(key(KeyCode::Char('G')));
    assert_eq!(b.selected(), 2);
    assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Noop);
    b.on_key(key(KeyCode::Char('g')));
    assert_eq!(b.selected(), 0);
}

#[test]
fn filter_matches_command_and_output_case_insensitively() {
    let mut b = sample();
    b.on_key(key(KeyCode::Char('/')));
    assert!(b.filter_input());
    for ch in "CARGO".chars() {
        b.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(commands(&b), vec!["cargo test", "cargo build"]);

    // "compiling" only appears in the output of `cargo build`.
    for _ in 0..5 {
        b.on_key(key(KeyCode::Backspace));
    }
    for ch in "compiling".chars() {
        b.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(commands(&b), vec!["cargo build"]);
}

#[test]
fn esc_during_filter_input_clears_it_instead_of_quitting() {
    let mut b = sample();
    b.on_key(key(KeyCode::Char('/')));
    for ch in "status".chars() {
        b.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(b.matched(), 1);

    assert_eq!(b.on_key(key(KeyCode::Esc)), BrowserAction::Redraw);
    assert!(!b.filter_input());
    assert_eq!(b.filter(), "");
    assert_eq!(b.matched(), 3);
}

#[test]
fn filter_input_captures_keys_that_are_commands_outside_it() {
    let mut b = sample();
    b.on_key(key(KeyCode::Char('/')));
    // 'q' would quit outside filter mode.
    assert_eq!(b.on_key(key(KeyCode::Char('q'))), BrowserAction::Redraw);
    assert_eq!(b.filter(), "q");
}

#[test]
fn failed_filter_excludes_zero_exit_blocks() {
    let mut b = sample();
    assert_eq!(b.on_key(key(KeyCode::Char('f'))), BrowserAction::Redraw);
    assert_eq!(commands(&b), vec!["cargo test"]);
    b.on_key(key(KeyCode::Char('f')));
    assert_eq!(b.matched(), 3);
}

#[test]
fn watched_filter_keeps_only_ai_watched_blocks() {
    let mut watched = block(4, "ai-watch -- make", 0, "out");
    watched.watched = true;
    let mut b = BlockBrowser::new(vec![block(1, "ls", 0, ""), watched]);

    b.on_key(key(KeyCode::Char('w')));
    assert_eq!(commands(&b), vec!["ai-watch -- make"]);
}

#[test]
fn selection_clamps_when_the_filter_shrinks_the_list() {
    let mut b = sample();
    b.on_key(key(KeyCode::Char('G')));
    assert_eq!(b.selected(), 2);

    b.on_key(key(KeyCode::Char('f'))); // one failed block
    assert_eq!(b.matched(), 1);
    assert_eq!(b.selected(), 0);
    assert_eq!(b.selected_block().unwrap().command, "cargo test");
}

#[test]
fn tab_switches_focus_so_movement_scrolls_the_output() {
    let mut b = BlockBrowser::new(vec![block(1, "seq", 0, "1\n2\n3\n4\n5\n6")]);
    assert_eq!(b.focus(), Focus::List);

    b.on_key(key(KeyCode::Tab));
    assert_eq!(b.focus(), Focus::Output);
    assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Redraw);
    assert_eq!(b.output_scroll(), 1);
    // Selection did not move.
    assert_eq!(b.selected(), 0);
}

#[test]
fn output_scroll_stops_at_the_last_line() {
    let mut b = BlockBrowser::new(vec![block(1, "seq", 0, "1\n2\n3")]);
    b.on_key(key(KeyCode::Tab));
    b.on_key(key(KeyCode::Char('G')));
    assert_eq!(b.output_scroll(), 2);
    assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Noop);
}

#[test]
fn page_keys_scroll_by_the_pane_height() {
    let output: String = (0..50).map(|i| format!("line {i}\n")).collect();
    let mut b = BlockBrowser::new(vec![block(1, "seq", 0, &output)]);
    b.set_output_height(20);

    b.on_key(ctrl('d'));
    assert_eq!(b.output_scroll(), 20);
    b.on_key(ctrl('u'));
    assert_eq!(b.output_scroll(), 0);
}

#[test]
fn changing_selection_resets_the_output_scroll() {
    let mut b = BlockBrowser::new(vec![
        block(1, "a", 0, "1\n2\n3\n4\n5"),
        block(2, "b", 0, "x\ny\nz"),
    ]);
    b.on_key(key(KeyCode::Tab));
    b.on_key(key(KeyCode::Char('j')));
    assert_eq!(b.output_scroll(), 1);

    b.on_key(key(KeyCode::Tab)); // back to the list
    b.on_key(key(KeyCode::Char('j')));
    assert_eq!(b.output_scroll(), 0);
}

#[test]
fn clamp_scroll_pulls_a_stale_offset_back_into_range() {
    let mut b = BlockBrowser::new(vec![block(1, "seq", 0, "1\n2\n3")]);
    b.on_key(key(KeyCode::Tab));
    b.on_key(key(KeyCode::Char('G')));
    assert_eq!(b.output_scroll(), 2);

    b.clamp_scroll();
    assert!(b.output_scroll() <= 2);
}

#[test]
fn fold_collapses_output_to_the_configured_line_count() {
    let output: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let mut b = BlockBrowser::new(vec![block(1, "seq", 0, &output)]);

    let (lines, hidden) = b.output_lines();
    assert_eq!(lines.len(), 20);
    assert_eq!(hidden, 0);

    b.on_key(key(KeyCode::Char(' ')));
    assert!(b.is_folded());
    let (lines, hidden) = b.output_lines();
    assert_eq!(lines.len(), FOLDED_LINES);
    assert_eq!(hidden, 20 - FOLDED_LINES);

    b.on_key(key(KeyCode::Char(' ')));
    assert!(!b.is_folded());
}

#[test]
fn short_output_is_not_folded_even_when_marked() {
    let mut b = BlockBrowser::new(vec![block(1, "x", 0, "one\ntwo")]);
    b.on_key(key(KeyCode::Char(' ')));
    let (lines, hidden) = b.output_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(hidden, 0);
}

#[test]
fn full_screen_program_output_starts_folded() {
    // A vim-like redraw is almost entirely cursor positioning.
    let noisy: String = (0..40)
        .map(|row| format!("\x1b[{};1H\x1b[K~\n", row))
        .collect();
    let b = BlockBrowser::new(vec![block(1, "vim", 0, &noisy)]);
    assert!(b.is_folded());
}

#[test]
fn plain_output_does_not_start_folded() {
    assert!(!sample().is_folded());
}

#[test]
fn stream_toggle_cycles_and_selects_the_right_text() {
    let mut blk = block(1, "cmd", 1, "on stdout");
    blk.stderr = "on stderr".to_string();
    let mut b = BlockBrowser::new(vec![blk]);

    assert_eq!(b.stream(), OutputStream::Both);
    assert_eq!(b.output_lines().0, vec!["on stdout", "on stderr"]);

    b.on_key(key(KeyCode::Char('s')));
    assert_eq!(b.stream(), OutputStream::Stdout);
    assert_eq!(b.output_lines().0, vec!["on stdout"]);

    b.on_key(key(KeyCode::Char('s')));
    assert_eq!(b.stream(), OutputStream::Stderr);
    assert_eq!(b.output_lines().0, vec!["on stderr"]);

    b.on_key(key(KeyCode::Char('s')));
    assert_eq!(b.stream(), OutputStream::Both);
}

#[test]
fn output_is_ansi_stripped_and_progress_collapsed() {
    let b = &mut BlockBrowser::new(vec![block(
        1,
        "build",
        0,
        "\x1b[32m1/3\r2/3\r3/3\x1b[0m\ndone\n",
    )]);
    assert_eq!(b.output_lines().0, vec!["3/3", "done"]);
}

#[test]
fn enter_inserts_and_r_runs_the_command() {
    let mut b = sample();
    assert_eq!(
        b.on_key(key(KeyCode::Enter)),
        BrowserAction::Finish(BrowserOutcome::Insert("git status".to_string()))
    );
    assert_eq!(
        b.on_key(key(KeyCode::Char('r'))),
        BrowserAction::Finish(BrowserOutcome::Run("git status".to_string()))
    );
}

#[test]
fn d_jumps_to_the_directory_the_block_ran_in() {
    let mut b = sample();
    assert_eq!(
        b.on_key(key(KeyCode::Char('d'))),
        BrowserAction::Finish(BrowserOutcome::Run("cd /repo".to_string()))
    );
}

#[test]
fn d_quotes_directories_that_need_it() {
    let mut blk = block(1, "ls", 0, "");
    blk.cwd = Some("/tmp/my project".to_string());
    let mut b = BlockBrowser::new(vec![blk]);
    assert_eq!(
        b.on_key(key(KeyCode::Char('d'))),
        BrowserAction::Finish(BrowserOutcome::Run("cd '/tmp/my project'".to_string()))
    );
}

#[test]
fn d_without_a_recorded_directory_does_nothing() {
    let mut blk = block(1, "ls", 0, "");
    blk.cwd = None;
    let mut b = BlockBrowser::new(vec![blk]);
    assert_eq!(b.on_key(key(KeyCode::Char('d'))), BrowserAction::Noop);
}

#[test]
fn e_routes_explanation_through_the_blocks_builtin() {
    // An AI call cannot happen inside the synchronous closure, so it goes
    // back to the shell as a command.
    let mut b = sample();
    b.on_key(key(KeyCode::Char('j')));
    assert_eq!(
        b.on_key(key(KeyCode::Char('e'))),
        BrowserAction::Finish(BrowserOutcome::Run("blocks explain 2".to_string()))
    );
}

#[test]
fn explain_numbers_against_the_unfiltered_list() {
    // `blocks explain N` indexes get_all_blocks(); using the position within
    // the filter would explain a different block entirely.
    let mut b = sample();
    b.on_key(key(KeyCode::Char('f'))); // only "cargo test" survives
    assert_eq!(b.matched(), 1);
    assert_eq!(b.selected(), 0);
    assert_eq!(b.selected_block().unwrap().command, "cargo test");

    // "cargo test" is the 2nd entry of the full list, not the 1st.
    assert_eq!(
        b.on_key(key(KeyCode::Char('e'))),
        BrowserAction::Finish(BrowserOutcome::Run("blocks explain 2".to_string()))
    );
}

#[test]
fn c_copies_the_command_and_y_copies_the_output() {
    let mut b = sample();
    assert_eq!(
        b.on_key(key(KeyCode::Char('c'))),
        BrowserAction::Copy("git status".to_string())
    );
    assert_eq!(
        b.on_key(key(KeyCode::Char('y'))),
        BrowserAction::Copy("clean".to_string())
    );
}

#[test]
fn y_with_no_output_does_nothing() {
    let mut b = BlockBrowser::new(vec![block(1, "cd /tmp", 0, "")]);
    assert_eq!(b.on_key(key(KeyCode::Char('y'))), BrowserAction::Noop);
}

#[test]
fn q_and_esc_and_ctrl_c_quit() {
    for k in [key(KeyCode::Char('q')), key(KeyCode::Esc), ctrl('c')] {
        let mut b = sample();
        assert_eq!(b.on_key(k), BrowserAction::Finish(BrowserOutcome::Quit));
    }
}

#[test]
fn help_opens_and_the_next_key_dismisses_it() {
    let mut b = sample();
    b.on_key(key(KeyCode::Char('?')));
    assert!(b.show_help());

    // Dismissing must not also trigger the key's normal action.
    assert_eq!(b.on_key(key(KeyCode::Char('r'))), BrowserAction::Redraw);
    assert!(!b.show_help());
}

#[test]
fn empty_output_explains_why_rather_than_showing_a_blank_pane() {
    let b = BlockBrowser::new(vec![block(1, "cd /tmp", 0, "")]);
    assert!(b.empty_output_note().is_some());

    let b = BlockBrowser::new(vec![block(1, "ls", 0, "file")]);
    assert!(b.empty_output_note().is_none());
}

#[test]
fn truncation_is_surfaced_so_the_tail_is_not_misread() {
    let b = BlockBrowser::new(vec![block(1, "big", 0, "... (truncated)\nlast lines")]);
    assert!(b.is_truncated());
    assert!(!sample().is_truncated());
}

#[test]
fn an_empty_block_list_has_nothing_selected() {
    let b = BlockBrowser::new(Vec::new());
    assert!(b.is_empty());
    assert_eq!(b.matched(), 0);
    assert!(b.selected_block().is_none());
}

#[test]
fn keys_on_an_empty_list_do_not_finish_with_a_command() {
    let mut b = BlockBrowser::new(Vec::new());
    for k in [
        key(KeyCode::Enter),
        key(KeyCode::Char('r')),
        key(KeyCode::Char('d')),
        key(KeyCode::Char('e')),
        key(KeyCode::Char('c')),
        key(KeyCode::Char('y')),
    ] {
        assert_eq!(b.on_key(k), BrowserAction::Noop);
    }
}

#[test]
fn quote_path_only_quotes_when_needed() {
    assert_eq!(quote_path("/repo/src"), "/repo/src");
    assert_eq!(quote_path("/tmp/my project"), "'/tmp/my project'");
    assert_eq!(quote_path("/tmp/it's"), r"'/tmp/it'\''s'");
    assert_eq!(quote_path("/tmp/$HOME"), "'/tmp/$HOME'");
    assert_eq!(quote_path(""), "''");
}

#[test]
fn status_message_clears_on_the_next_key() {
    let mut b = sample();
    b.set_status("copied");
    assert_eq!(b.status(), Some("copied"));
    b.on_key(key(KeyCode::Char('j')));
    assert!(b.status().is_none());
}
