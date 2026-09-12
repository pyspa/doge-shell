use super::*;

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn ctx_default() -> KeyContext {
    KeyContext {
        cursor_at_end: true,
        input_empty: false,
        has_suggestion: false,
        has_completion: false,
        completion_mode: false,
        cursor_at_start: false,
        next_char: None,
        auto_pair: false,
        multiline_active: false,
    }
}

// === Input shortcuts (Alt+. / Alt+; / Alt+n / Alt+p) ===

#[test]
fn alt_dot_and_alt_underscore_insert_the_last_argument() {
    let ctx = ctx_default();
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('.'), ALT), &ctx),
        KeyAction::InsertLastArgument
    );
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('_'), ALT), &ctx),
        KeyAction::InsertLastArgument
    );
}

/// `Ctrl+_` is undo and must not be captured by the `Alt+_` arm above it.
#[test]
fn ctrl_underscore_is_still_undo() {
    let ctx = ctx_default();
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('_'), CTRL), &ctx),
        KeyAction::Undo
    );
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('/'), CTRL), &ctx),
        KeyAction::Undo
    );
}

#[test]
fn snippet_and_placeholder_keys_are_bound() {
    let ctx = ctx_default();
    assert_eq!(
        determine_key_action(&key(KeyCode::Char(';'), ALT), &ctx),
        KeyAction::InsertSnippet
    );
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('n'), ALT), &ctx),
        KeyAction::NextPlaceholder
    );
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('p'), ALT), &ctx),
        KeyAction::PrevPlaceholder
    );
}

/// The new ALT bindings must not shadow plain typing.
#[test]
fn unmodified_versions_of_the_new_keys_still_insert_characters() {
    let ctx = ctx_default();
    for ch in ['.', ';', 'n', 'p', '_'] {
        assert_eq!(
            determine_key_action(&key(KeyCode::Char(ch), NONE), &ctx),
            KeyAction::InsertChar(ch),
            "plain '{ch}' should still be typed"
        );
    }
}

// === Ctrl-D / Delete / Home / End / Ctrl-Z ===

#[test]
fn ctrl_d_on_empty_buffer_is_eof() {
    let ctx = KeyContext {
        input_empty: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('d'), CTRL), &ctx),
        KeyAction::Eof
    );
}

#[test]
fn ctrl_d_with_text_deletes_forward() {
    let ctx = KeyContext {
        input_empty: false,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('d'), CTRL), &ctx),
        KeyAction::DeleteCharForward
    );
}

#[test]
fn ctrl_d_in_multiline_is_not_eof() {
    // An empty continuation line must not exit the shell.
    let ctx = KeyContext {
        input_empty: true,
        multiline_active: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('d'), CTRL), &ctx),
        KeyAction::DeleteCharForward
    );
}

#[test]
fn delete_key_deletes_forward() {
    assert_eq!(
        determine_key_action(&key(KeyCode::Delete, NONE), &ctx_default()),
        KeyAction::DeleteCharForward
    );
}

#[test]
fn home_and_end_move_cursor() {
    assert_eq!(
        determine_key_action(&key(KeyCode::Home, NONE), &ctx_default()),
        KeyAction::CursorToBegin
    );
    assert_eq!(
        determine_key_action(&key(KeyCode::End, NONE), &ctx_default()),
        KeyAction::CursorToEnd
    );
}

#[test]
fn ctrl_z_on_empty_buffer_resumes_job() {
    let ctx = KeyContext {
        input_empty: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('z'), CTRL), &ctx),
        KeyAction::ResumeLastJob
    );
}

#[test]
fn ctrl_z_with_text_is_unsupported() {
    // Left free so a future undo binding can claim it.
    let ctx = KeyContext {
        input_empty: false,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('z'), CTRL), &ctx),
        KeyAction::Unsupported
    );
}

#[test]
fn ctrl_o_opens_the_block_browser() {
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('o'), CTRL), &ctx_default()),
        KeyAction::OpenBlockBrowser
    );
}

#[test]
fn ctrl_o_is_not_shadowed_by_completion_or_suggestion_context() {
    let ctx = KeyContext {
        has_completion: true,
        has_suggestion: true,
        completion_mode: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('o'), CTRL), &ctx),
        KeyAction::OpenBlockBrowser
    );
}

// === Kill ring / undo ===

#[test]
fn ctrl_y_yanks() {
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('y'), CTRL), &ctx_default()),
        KeyAction::Yank
    );
}

#[test]
fn ctrl_underscore_undoes() {
    // The 0x1F byte a legacy terminal actually sends: crossterm decodes it
    // as Ctrl+7, not Ctrl+_.
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('7'), CTRL), &ctx_default()),
        KeyAction::Undo
    );
    // Kitty-keyboard-protocol terminals report the literal key instead.
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('_'), CTRL), &ctx_default()),
        KeyAction::Undo
    );
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('/'), CTRL), &ctx_default()),
        KeyAction::Undo
    );
    // Some terminals add SHIFT for the underscore.
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('_'), CTRL | SHIFT), &ctx_default()),
        KeyAction::Undo
    );
}

#[test]
fn alt_slash_redoes() {
    assert_eq!(
        determine_key_action(&key(KeyCode::Char('/'), ALT), &ctx_default()),
        KeyAction::Redo
    );
}

// === Cursor movement tests ===

#[test]
fn test_ctrl_a_moves_to_begin() {
    let k = key(KeyCode::Char('a'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::CursorToBegin
    );
}

#[test]
fn test_ctrl_e_moves_to_end() {
    let k = key(KeyCode::Char('e'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::CursorToEnd
    );
}

#[test]
fn test_left_arrow_moves_cursor_left() {
    let k = key(KeyCode::Left, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::CursorLeft
    );
}

#[test]
fn test_right_arrow_moves_cursor_right() {
    let k = key(KeyCode::Right, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::CursorRight
    );
}

#[test]
fn test_ctrl_left_moves_word_left() {
    let k = key(KeyCode::Left, CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::CursorWordLeft
    );
}

#[test]
fn test_ctrl_right_moves_word_right() {
    let k = key(KeyCode::Right, CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::CursorWordRight
    );
}

// === Editing operations tests ===

#[test]
fn test_backspace() {
    let k = key(KeyCode::Backspace, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::Backspace
    );
}

#[test]
fn test_ctrl_w_deletes_word() {
    let k = key(KeyCode::Char('w'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::DeleteWordBackward
    );
}

#[test]
fn test_ctrl_k_deletes_to_end() {
    let k = key(KeyCode::Char('k'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::DeleteToEnd
    );
}

#[test]
fn test_ctrl_u_deletes_to_beginning() {
    let k = key(KeyCode::Char('u'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::DeleteToBeginning
    );
}

// === Character input tests ===

#[test]
fn test_regular_char_inserts() {
    let k = key(KeyCode::Char('a'), NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::InsertChar('a')
    );
}

#[test]
fn test_shift_char_inserts() {
    let k = key(KeyCode::Char('A'), SHIFT);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::InsertChar('A')
    );
}

#[test]
fn test_open_paren_inserts_pair_when_enabled() {
    let k = key(KeyCode::Char('('), NONE);
    let ctx = KeyContext {
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::InsertPairedChar {
            open: '(',
            close: ')'
        }
    );
}

#[test]
fn test_open_paren_inserts_single_when_disabled() {
    let k = key(KeyCode::Char('('), NONE);
    let ctx = KeyContext {
        auto_pair: false,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::InsertChar('('));
}

#[test]
fn test_open_brace_inserts_pair_when_enabled() {
    let k = key(KeyCode::Char('{'), NONE);
    let ctx = KeyContext {
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::InsertPairedChar {
            open: '{',
            close: '}'
        }
    );
}

#[test]
fn test_close_paren_overtypes_when_enabled_and_matching() {
    let k = key(KeyCode::Char(')'), NONE);
    let ctx = KeyContext {
        next_char: Some(')'),
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::OvertypeClosingBracket(')')
    );
}

#[test]
fn test_close_paren_inserts_when_disabled() {
    let k = key(KeyCode::Char(')'), NONE);
    let ctx = KeyContext {
        next_char: Some(')'),
        auto_pair: false,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::InsertChar(')'));
}

#[test]
fn test_close_paren_inserts_when_not_matching() {
    let k = key(KeyCode::Char(')'), NONE);
    let ctx = KeyContext {
        next_char: Some('x'),
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::InsertChar(')'));
}

// === Command execution tests ===

#[test]
fn test_enter_executes() {
    let k = key(KeyCode::Enter, NONE);
    assert_eq!(determine_key_action(&k, &ctx_default()), KeyAction::Execute);
}

#[test]
fn test_alt_enter_executes_background() {
    let k = key(KeyCode::Enter, ALT);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::ExecuteBackground
    );
}

// === Completion tests ===

#[test]
fn test_tab_triggers_completion() {
    let k = key(KeyCode::Tab, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::TriggerCompletion
    );
}

// === History tests ===

#[test]
fn test_up_is_history_previous() {
    let k = key(KeyCode::Up, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::HistoryPrevious
    );
}

#[test]
fn test_ctrl_r_is_history_search() {
    let k = key(KeyCode::Char('r'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::HistorySearch
    );
}

// === Suggestion tests ===

#[test]
fn test_right_accepts_suggestion_when_active() {
    let k = key(KeyCode::Right, NONE);
    let ctx = KeyContext {
        cursor_at_end: true,
        has_suggestion: true,
        has_completion: false,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::AcceptSuggestionFull
    );
}

#[test]
fn test_ctrl_right_accepts_suggestion_word() {
    let k = key(KeyCode::Right, CTRL);
    let ctx = KeyContext {
        cursor_at_end: true,
        has_suggestion: true,
        has_completion: false,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::AcceptSuggestionWord
    );
}

#[test]
fn test_alt_bracket_rotates_suggestion() {
    let k1 = key(KeyCode::Char(']'), ALT);
    let k2 = key(KeyCode::Char('['), ALT);
    assert_eq!(
        determine_key_action(&k1, &ctx_default()),
        KeyAction::RotateSuggestionForward
    );
    assert_eq!(
        determine_key_action(&k2, &ctx_default()),
        KeyAction::RotateSuggestionBackward
    );
}

// === Other tests ===

#[test]
fn test_ctrl_c_interrupt() {
    let k = key(KeyCode::Char('c'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::Interrupt
    );
}

#[test]
fn test_ctrl_l_clear_screen() {
    let k = key(KeyCode::Char('l'), CTRL);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::ClearScreen
    );
}

#[test]
fn test_ctrl_v_paste() {
    let k = key(KeyCode::Char('v'), CTRL);
    assert_eq!(determine_key_action(&k, &ctx_default()), KeyAction::Paste);
}

#[test]
fn test_esc_cancels_completion_when_active() {
    let k = key(KeyCode::Esc, NONE);
    let ctx = KeyContext {
        has_completion: true,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::CancelCompletion);
}

#[test]
fn test_esc_toggles_sudo_when_no_completion() {
    let k = key(KeyCode::Esc, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::ToggleSudo
    );
}

#[test]
fn test_space_triggers_abbreviation_check() {
    let k = key(KeyCode::Char(' '), NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::ExpandAbbreviationAndInsertSpace
    );
}

// === Context dependent tests ===

#[test]
fn test_ctrl_e_accepts_completion_when_active() {
    let k = key(KeyCode::Char('e'), CTRL);
    let ctx = KeyContext {
        has_completion: true,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::AcceptCompletion);
}

#[test]
fn test_ctrl_e_moves_to_end_when_no_completion() {
    let k = key(KeyCode::Char('e'), CTRL);
    let ctx = KeyContext {
        has_completion: false,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::CursorToEnd);
}

#[test]
fn test_alt_f_is_ai_autofix_when_no_suggestion() {
    let k = key(KeyCode::Char('f'), ALT);
    let ctx = KeyContext {
        has_suggestion: false,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::AiAutoFix);
}

#[test]
fn test_alt_f_accepts_suggestion_word_when_active() {
    let k = key(KeyCode::Char('f'), ALT);
    let ctx = KeyContext {
        cursor_at_end: true,
        has_suggestion: true,
        has_completion: false,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::AcceptSuggestionWord
    );
}

// === Edge case tests ===

#[test]
fn test_right_moves_cursor_when_not_at_end() {
    let k = key(KeyCode::Right, NONE);
    let ctx = KeyContext {
        cursor_at_end: false,
        has_suggestion: true, // Move if cursor is not at end even if suggestion exists
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::CursorRight);
}

#[test]
fn test_ctrl_right_moves_word_when_no_suggestion() {
    let k = key(KeyCode::Right, CTRL);
    let ctx = KeyContext {
        cursor_at_end: true,
        has_suggestion: false,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::CursorWordRight);
}

#[test]
fn test_open_bracket_inserts_pair_when_enabled() {
    let k = key(KeyCode::Char('['), NONE);
    let ctx = KeyContext {
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::InsertPairedChar {
            open: '[',
            close: ']'
        }
    );
}

#[test]
fn test_single_quote_inserts_pair_when_enabled() {
    let k = key(KeyCode::Char('\''), NONE);
    let ctx = KeyContext {
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::InsertPairedChar {
            open: '\'',
            close: '\''
        }
    );
}

#[test]
fn test_double_quote_inserts_pair_when_enabled() {
    let k = key(KeyCode::Char('"'), NONE);
    let ctx = KeyContext {
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::InsertPairedChar {
            open: '"',
            close: '"'
        }
    );
}

#[test]
fn test_close_bracket_overtypes() {
    let k = key(KeyCode::Char(']'), NONE);
    let ctx = KeyContext {
        next_char: Some(']'),
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::OvertypeClosingBracket(']')
    );
}

#[test]
fn test_close_brace_overtypes() {
    let k = key(KeyCode::Char('}'), NONE);
    let ctx = KeyContext {
        next_char: Some('}'),
        auto_pair: true,
        ..ctx_default()
    };
    assert_eq!(
        determine_key_action(&k, &ctx),
        KeyAction::OvertypeClosingBracket('}')
    );
}

#[test]
fn test_down_is_history_next_in_completion_mode() {
    let k = key(KeyCode::Down, NONE);
    let ctx = KeyContext {
        completion_mode: true,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::HistoryNext);
}

#[test]
fn test_down_is_history_next_outside_completion_mode() {
    let k = key(KeyCode::Down, NONE);
    let ctx = KeyContext {
        completion_mode: false,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::HistoryNext);
}

#[test]
fn test_backtab_triggers_completion() {
    let k = key(KeyCode::BackTab, NONE);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::TriggerCompletion
    );
}

#[test]
fn test_right_accepts_completion_when_active() {
    let k = key(KeyCode::Right, NONE);
    let ctx = KeyContext {
        cursor_at_end: true,
        has_completion: true,
        ..ctx_default()
    };
    assert_eq!(determine_key_action(&k, &ctx), KeyAction::AcceptCompletion);
}

#[test]
fn test_alt_s_is_force_ai_suggestion() {
    let k = key(KeyCode::Char('s'), ALT);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::ForceAiSuggestion
    );
}

#[test]
fn test_alt_e_is_ai_explain_command() {
    let k = key(KeyCode::Char('e'), ALT);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::AiExplainCommand
    );
}

#[test]
fn test_alt_w_is_ai_watch_current_input() {
    let k = key(KeyCode::Char('w'), ALT);
    assert_eq!(
        determine_key_action(&k, &ctx_default()),
        KeyAction::AiWatchCurrentInput
    );
}
