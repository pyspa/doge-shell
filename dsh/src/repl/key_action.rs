//! キー入力アクションの定義と純粋なマッピング関数
//!
//! このモジュールはキー入力と操作のマッピングを純粋関数として分離し、
//! 副作用なしでテスト可能にする。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

/// キー入力に対応するアクション
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    // カーソル移動
    CursorLeft,
    CursorRight,
    CursorWordLeft,
    CursorWordRight,
    CursorToBegin,
    CursorToEnd,

    // 履歴ナビゲーション
    HistoryPrevious,
    HistoryNext,
    HistorySearch,

    // 編集操作
    InsertChar(char),
    InsertPairedChar { open: char, close: char },
    Backspace,
    DeleteWordBackward,
    DeleteToEnd,
    DeleteToBeginning,

    // 補完・サジェスト
    TriggerCompletion,
    AcceptCompletion,
    AcceptSuggestionFull,
    AcceptSuggestionWord,
    RotateSuggestionForward,
    RotateSuggestionBackward,

    // コマンド実行
    Execute,
    ExecuteBackground,

    // Command Palette
    OpenCommandPalette,

    // AI機能
    AiAutoFix,
    AiSmartCommit,
    AiDiagnose,
    ForceAiSuggestion,

    // その他
    Paste,
    OpenEditor, // Open editor for current input
    ClearScreen,
    Interrupt,
    ToggleSudo,
    CancelCompletion,

    // 特殊処理（context依存）
    OvertypeClosingBracket(char),
    ExpandAbbreviationAndInsertSpace,

    // 無視
    Unsupported,
}

/// キー入力時のコンテキスト（純粋関数への入力）
#[derive(Debug, Clone, Default)]
pub struct KeyContext {
    /// カーソルが入力の末尾にあるか
    pub cursor_at_end: bool,
    /// 入力が空か
    pub input_empty: bool,
    /// サジェストがアクティブか
    pub has_suggestion: bool,
    /// 補完がアクティブか（input.completion.is_some()）
    pub has_completion: bool,
    /// 補完モードか（completion.completion_mode()）
    pub completion_mode: bool,
    /// カーソル位置が0か
    pub cursor_at_start: bool,
    /// 次の文字（OvertypeClosingBracket用）
    pub next_char: Option<char>,
    /// オートペアが有効か
    pub auto_pair: bool,
}

/// キー入力からアクションを決定（純粋関数）
///
/// この関数は副作用を持たず、キー入力とコンテキストからアクションを決定する。
/// テスト容易性のため、状態変更は行わない。
pub fn determine_key_action(key: &KeyEvent, ctx: &KeyContext) -> KeyAction {
    match (key.code, key.modifiers) {
        // 履歴ナビゲーション
        (KeyCode::Up, NONE) => KeyAction::HistoryPrevious,
        (KeyCode::Down, NONE) => KeyAction::HistoryNext,

        // サジェスト受け入れ（Ctrl+Right, Alt+f で単語単位）
        (KeyCode::Right, m)
            if m.contains(CTRL)
                && ctx.has_suggestion
                && !ctx.has_completion
                && ctx.cursor_at_end =>
        {
            KeyAction::AcceptSuggestionWord
        }
        (KeyCode::Char('f'), ALT)
            if ctx.has_suggestion && !ctx.has_completion && ctx.cursor_at_end =>
        {
            KeyAction::AcceptSuggestionWord
        }

        // サジェストのローテーション
        (KeyCode::Char(']'), ALT) => KeyAction::RotateSuggestionForward,
        (KeyCode::Char('['), ALT) => KeyAction::RotateSuggestionBackward,

        // カーソル移動（Left）
        (KeyCode::Left, m) if !m.contains(CTRL) => KeyAction::CursorLeft,
        (KeyCode::Left, m) if m.contains(CTRL) => KeyAction::CursorWordLeft,

        // サジェスト受け入れ（Right で全体）
        (KeyCode::Right, m)
            if ctx.has_suggestion
                && !ctx.has_completion
                && ctx.cursor_at_end
                && !m.contains(CTRL) =>
        {
            KeyAction::AcceptSuggestionFull
        }

        // 補完（ゴーストテキスト）受け入れ（Right）
        (KeyCode::Right, m) if ctx.has_completion && ctx.cursor_at_end && !m.contains(CTRL) => {
            KeyAction::AcceptCompletion
        }

        // カーソル移動（Right）
        (KeyCode::Right, m) if !m.contains(CTRL) => KeyAction::CursorRight,
        (KeyCode::Right, m) if m.contains(CTRL) => KeyAction::CursorWordRight,

        // Ctrl+f でサジェスト受け入れ
        (KeyCode::Char('f'), CTRL)
            if ctx.has_suggestion && !ctx.has_completion && ctx.cursor_at_end =>
        {
            KeyAction::AcceptSuggestionFull
        }

        // スペース入力（略語展開チェック）
        (KeyCode::Char(' '), NONE) => KeyAction::ExpandAbbreviationAndInsertSpace,

        // Auto-pairing: 開き括弧
        (KeyCode::Char(ch), NONE)
            if ctx.auto_pair && matches!(ch, '(' | '{' | '[' | '\'' | '"') =>
        {
            let close = match ch {
                '(' => ')',
                '{' => '}',
                '[' => ']',
                '\'' => '\'',
                '"' => '"',
                _ => ch,
            };
            KeyAction::InsertPairedChar { open: ch, close }
        }

        // Overtype: 閉じ括弧
        (KeyCode::Char(ch), NONE) if ctx.auto_pair && matches!(ch, ')' | '}' | ']') => {
            if ctx.next_char == Some(ch) {
                KeyAction::OvertypeClosingBracket(ch)
            } else {
                KeyAction::InsertChar(ch)
            }
        }

        // クオートのオーバータイプ
        (KeyCode::Char(ch), NONE) if ctx.auto_pair && matches!(ch, '\'' | '"') => {
            KeyAction::InsertChar(ch)
        }

        // 通常の文字入力
        (KeyCode::Char(ch), NONE) => KeyAction::InsertChar(ch),
        (KeyCode::Char(ch), SHIFT) => KeyAction::InsertChar(ch),

        // Backspace
        (KeyCode::Backspace, NONE) => KeyAction::Backspace,

        // AI機能
        (KeyCode::Char('f'), ALT) => KeyAction::AiAutoFix,
        (KeyCode::Char('s'), ALT) => KeyAction::ForceAiSuggestion,
        (KeyCode::Char('c'), ALT) => KeyAction::AiSmartCommit,
        (KeyCode::Char('d'), ALT) => KeyAction::AiDiagnose,

        // Tab: 補完
        (KeyCode::Tab, NONE) | (KeyCode::BackTab, NONE) => KeyAction::TriggerCompletion,

        // Enter: 実行
        (KeyCode::Enter, NONE) => KeyAction::Execute,
        (KeyCode::Enter, ALT) => KeyAction::ExecuteBackground,

        // Alt+x: Command Palette
        (KeyCode::Char('x'), ALT) => KeyAction::OpenCommandPalette,

        // 行頭・行末移動
        (KeyCode::Char('a'), CTRL) => KeyAction::CursorToBegin,
        // Ctrl+E: 補完がある場合は受け入れ、ない場合は行末移動
        (KeyCode::Char('e'), CTRL) if ctx.has_completion => KeyAction::AcceptCompletion,
        (KeyCode::Char('e'), CTRL) => KeyAction::CursorToEnd,

        // Ctrl+C: 割り込み
        (KeyCode::Char('c'), CTRL) => KeyAction::Interrupt,

        // Ctrl+L: 画面クリア
        (KeyCode::Char('l'), CTRL) => KeyAction::ClearScreen,

        // Ctrl+D: 通常は何もしない（exitメッセージ表示）
        (KeyCode::Char('d'), CTRL) => KeyAction::Unsupported,

        // Ctrl+R: 履歴検索
        (KeyCode::Char('r'), CTRL) => KeyAction::HistorySearch,

        // Ctrl+V: ペースト
        (KeyCode::Char('v'), CTRL) => KeyAction::Paste,

        // Ctrl+W: 単語削除
        (KeyCode::Char('w'), CTRL) => KeyAction::DeleteWordBackward,

        // Ctrl+K: 行末まで削除
        (KeyCode::Char('k'), CTRL) => KeyAction::DeleteToEnd,

        // Ctrl+U: 行頭まで削除
        (KeyCode::Char('u'), CTRL) => KeyAction::DeleteToBeginning,

        // Esc: 補完キャンセル or sudo切り替え
        (KeyCode::Esc, NONE) => {
            if ctx.has_completion || ctx.has_suggestion {
                KeyAction::CancelCompletion
            } else {
                KeyAction::ToggleSudo
            }
        }

        // その他
        _ => KeyAction::Unsupported,
    }
}

#[cfg(test)]
mod tests {
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
        }
    }

    // === カーソル移動テスト ===

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

    // === 編集操作テスト ===

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

    // === 文字入力テスト ===

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

    // === コマンド実行テスト ===

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

    // === 補完テスト ===

    #[test]
    fn test_tab_triggers_completion() {
        let k = key(KeyCode::Tab, NONE);
        assert_eq!(
            determine_key_action(&k, &ctx_default()),
            KeyAction::TriggerCompletion
        );
    }

    // === 履歴テスト ===

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

    // === サジェストテスト ===

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

    // === その他テスト ===

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

    // === コンテキスト依存テスト ===

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

    // === エッジケーステスト ===

    #[test]
    fn test_right_moves_cursor_when_not_at_end() {
        let k = key(KeyCode::Right, NONE);
        let ctx = KeyContext {
            cursor_at_end: false,
            has_suggestion: true, // サジェストがあってもカーソルが末尾でなければ移動
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
}
