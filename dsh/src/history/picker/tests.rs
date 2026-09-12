use super::*;

const NOW: i64 = 1_700_000_000;

fn entry(command: &str, exit_code: Option<i32>, duration_ms: Option<u64>) -> Entry {
    Entry {
        entry: command.to_string(),
        when: NOW - 120,
        count: 1,
        context: Some("/repo".to_string()),
        exit_code,
        duration_ms,
        cwd: Some("/repo/src".to_string()),
        session_id: Some("session-a".to_string()),
        hostname: Some("host".to_string()),
    }
}

fn picker(entries: Vec<Entry>) -> HistoryPicker {
    let base = HistoryQuery {
        current_cwd: Some("/repo/src".to_string()),
        current_project: Some("/repo".to_string()),
        current_session_id: Some("session-a".to_string()),
        ..Default::default()
    };
    HistoryPicker::new(entries, base, String::new(), NOW)
}

fn sample() -> HistoryPicker {
    picker(vec![
        entry("cargo build", Some(0), Some(4200)),
        entry("cargo test", Some(1), Some(200)),
        entry("git status", Some(0), Some(30)),
    ])
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

fn commands(picker: &HistoryPicker) -> Vec<String> {
    picker.rows().into_iter().map(|row| row.command).collect()
}

#[test]
fn typing_narrows_results() {
    let mut p = sample();
    assert_eq!(p.matched(), 3);

    assert_eq!(p.on_key(key(KeyCode::Char('g'))), PickerAction::Redraw);
    assert_eq!(p.on_key(key(KeyCode::Char('i'))), PickerAction::Redraw);
    assert_eq!(commands(&p), vec!["git status"]);
}

#[test]
fn matching_is_case_insensitive() {
    let mut p = sample();
    for ch in "CARGO".chars() {
        p.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(p.matched(), 2);
}

#[test]
fn backspace_widens_results() {
    let mut p = sample();
    for ch in "git".chars() {
        p.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(p.matched(), 1);

    // One backspace leaves "gi", which still only matches git status.
    p.on_key(key(KeyCode::Backspace));
    assert_eq!(p.matched(), 1);

    p.on_key(key(KeyCode::Backspace));
    p.on_key(key(KeyCode::Backspace));
    assert_eq!(p.query(), "");
    assert_eq!(p.matched(), 3);
}

#[test]
fn backspace_on_empty_query_is_a_noop() {
    let mut p = sample();
    assert_eq!(p.on_key(key(KeyCode::Backspace)), PickerAction::Noop);
}

#[test]
fn ctrl_u_clears_the_query() {
    let mut p = sample();
    for ch in "git".chars() {
        p.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(p.on_key(ctrl('u')), PickerAction::Redraw);
    assert_eq!(p.query(), "");
    assert_eq!(p.matched(), 3);
}

#[test]
fn ctrl_r_cycles_scope_through_all_four() {
    let mut p = sample();
    assert!(p.header().contains("scope:global"));
    p.on_key(ctrl('r'));
    assert!(p.header().contains("scope:session"));
    p.on_key(ctrl('r'));
    assert!(p.header().contains("scope:cwd"));
    p.on_key(ctrl('r'));
    assert!(p.header().contains("scope:project"));
    p.on_key(ctrl('r'));
    assert!(p.header().contains("scope:global"));
}

#[test]
fn scope_cwd_excludes_entries_from_other_directories() {
    let mut elsewhere = entry("cargo bench", Some(0), Some(10));
    elsewhere.cwd = Some("/other".to_string());
    let mut p = picker(vec![entry("cargo build", Some(0), Some(10)), elsewhere]);
    assert_eq!(p.matched(), 2);

    p.on_key(ctrl('r')); // session
    p.on_key(ctrl('r')); // cwd
    assert_eq!(commands(&p), vec!["cargo build"]);
}

#[test]
fn ctrl_s_cycles_status_any_failure_success() {
    let mut p = sample();
    assert!(p.header().contains("status:any"));

    p.on_key(ctrl('s'));
    assert!(p.header().contains("status:failure"));
    assert_eq!(commands(&p), vec!["cargo test"]);

    p.on_key(ctrl('s'));
    assert!(p.header().contains("status:success"));
    assert_eq!(commands(&p), vec!["cargo build", "git status"]);

    p.on_key(ctrl('s'));
    assert!(p.header().contains("status:any"));
    assert_eq!(p.matched(), 3);
}

#[test]
fn ctrl_t_toggles_slow_filter() {
    let mut p = sample();
    assert!(p.header().contains("slow:off"));

    p.on_key(ctrl('t'));
    assert!(p.header().contains("slow:on"));
    // Only the 4.2s build clears the 1s threshold.
    assert_eq!(commands(&p), vec!["cargo build"]);

    p.on_key(ctrl('t'));
    assert_eq!(p.matched(), 3);
}

#[test]
fn selection_clamps_when_filter_shrinks_results() {
    let mut p = sample();
    p.on_key(key(KeyCode::End));
    assert_eq!(p.selected(), 2);

    // Narrow to a single match; the old index is out of range.
    for ch in "git".chars() {
        p.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(p.matched(), 1);
    assert_eq!(p.selected(), 0);
    assert_eq!(p.selected_entry().unwrap().entry, "git status");
}

#[test]
fn selection_moves_and_stops_at_the_ends() {
    let mut p = sample();
    assert_eq!(p.selected(), 0);
    assert_eq!(p.on_key(key(KeyCode::Up)), PickerAction::Noop);

    assert_eq!(p.on_key(key(KeyCode::Down)), PickerAction::Redraw);
    assert_eq!(p.selected(), 1);
    p.on_key(key(KeyCode::Down));
    assert_eq!(p.selected(), 2);
    assert_eq!(p.on_key(key(KeyCode::Down)), PickerAction::Noop);
}

#[test]
fn ctrl_p_and_ctrl_n_move_the_selection() {
    let mut p = sample();
    p.on_key(ctrl('n'));
    assert_eq!(p.selected(), 1);
    p.on_key(ctrl('p'));
    assert_eq!(p.selected(), 0);
}

#[test]
fn enter_returns_the_raw_multiline_command() {
    let mut p = picker(vec![entry("echo a\necho b", Some(0), None)]);
    // Rendered flat so the row layout survives...
    assert_eq!(commands(&p), vec!["echo a⏎echo b"]);
    // ...but the buffer gets the real command back.
    assert_eq!(
        p.on_key(key(KeyCode::Enter)),
        PickerAction::Accept("echo a\necho b".to_string())
    );
}

#[test]
fn enter_with_no_matches_does_not_accept() {
    let mut p = sample();
    for ch in "zzzz".chars() {
        p.on_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(p.matched(), 0);
    // Accepting here would wipe whatever the user had typed.
    assert_eq!(p.on_key(key(KeyCode::Enter)), PickerAction::Noop);
}

#[test]
fn esc_and_ctrl_c_and_ctrl_g_cancel() {
    let mut p = sample();
    assert_eq!(p.on_key(key(KeyCode::Esc)), PickerAction::Cancel);
    assert_eq!(p.on_key(ctrl('c')), PickerAction::Cancel);
    assert_eq!(p.on_key(ctrl('g')), PickerAction::Cancel);
}

#[test]
fn header_reports_filtered_and_total_counts() {
    let mut p = sample();
    assert!(p.header().contains("(3/3"));
    for ch in "git".chars() {
        p.on_key(key(KeyCode::Char(ch)));
    }
    assert!(p.header().contains("(1/3"));
}

#[test]
fn header_flags_that_metadata_is_last_run_only() {
    // The UNIQUE index on the command text means a scope filter matches the
    // last run, not every run; the UI must not imply otherwise.
    assert!(sample().header().contains("last run"));
}

#[test]
fn an_initial_query_is_applied_immediately() {
    let base = HistoryQuery::default();
    let p = HistoryPicker::new(
        vec![
            entry("cargo build", Some(0), None),
            entry("git status", Some(0), None),
        ],
        base,
        "git".to_string(),
        NOW,
    );
    assert_eq!(p.matched(), 1);
}

// === formatting helpers ===

#[test]
fn format_status_distinguishes_success_failure_and_unknown() {
    assert_eq!(format_status(Some(0)), "✔");
    assert_eq!(format_status(Some(2)), "✘2");
    assert_eq!(format_status(None), "·");
}

#[test]
fn format_duration_picks_a_readable_unit() {
    assert_eq!(format_duration(None), "-");
    assert_eq!(format_duration(Some(340)), "340ms");
    assert_eq!(format_duration(Some(1200)), "1.2s");
    assert_eq!(format_duration(Some(90_000)), "1m30s");
}

#[test]
fn format_relative_time_picks_the_widest_unit() {
    assert_eq!(format_relative_time(NOW - 10, NOW), "now");
    assert_eq!(format_relative_time(NOW - 180, NOW), "3m");
    assert_eq!(format_relative_time(NOW - 7200, NOW), "2h");
    assert_eq!(format_relative_time(NOW - 86_400 * 5, NOW), "5d");
    assert_eq!(format_relative_time(NOW - 86_400 * 800, NOW), "2y");
}

#[test]
fn format_relative_time_tolerates_clock_skew() {
    assert_eq!(format_relative_time(NOW + 500, NOW), "now");
}

#[test]
fn shorten_cwd_uses_home_relative_paths() {
    assert_eq!(shorten_cwd("/home/me/repo", Some("/home/me"), 40), "~/repo");
    assert_eq!(shorten_cwd("/home/me", Some("/home/me"), 40), "~");
    assert_eq!(shorten_cwd("/etc", Some("/home/me"), 40), "/etc");
}

#[test]
fn shorten_cwd_keeps_the_leaf_directory() {
    let out = shorten_cwd("/very/long/path/to/the/project", None, 12);
    assert!(out.starts_with('…'));
    assert!(out.ends_with("project"));
    assert!(display_width(&out) <= 12);
}

#[test]
fn shorten_cwd_does_not_split_multibyte_chars() {
    let out = shorten_cwd("/日本語/ディレクトリ/名前", None, 10);
    assert!(display_width(&out) <= 10);
    assert!(out.ends_with("名前"));
}

#[test]
fn columns_for_width_drops_cwd_before_duration() {
    let wide = columns_for_width(120);
    assert!(wide.status && wide.duration && wide.age && wide.cwd);

    let medium = columns_for_width(80);
    assert!(medium.status && medium.duration && medium.age);
    assert!(!medium.cwd);

    let narrow = columns_for_width(40);
    assert!(narrow.status);
    assert!(!narrow.duration && !narrow.age && !narrow.cwd);

    let tiny = columns_for_width(16);
    assert!(!tiny.status && !tiny.duration && !tiny.age && !tiny.cwd);
}

#[test]
fn rows_drop_columns_on_a_narrow_terminal() {
    let mut p = sample();
    p.set_width(40);
    let row = &p.rows()[0];
    // Status and the command itself always survive.
    assert!(!row.status.is_empty());
    assert!(!row.command.is_empty());
    assert!(row.duration.is_empty());
    assert!(row.cwd.is_empty());
}

#[test]
fn empty_history_produces_no_rows_and_no_selection() {
    let p = picker(Vec::new());
    assert_eq!(p.matched(), 0);
    assert!(p.rows().is_empty());
    assert!(p.selected_entry().is_none());
}
