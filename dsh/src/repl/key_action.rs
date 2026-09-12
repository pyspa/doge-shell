//! Key input action definitions and pure mapping functions
//!
//! This module separates key input and operation mapping as pure functions,
//! making them testable without side effects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

/// Action corresponding to key input
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    // Cursor movement
    CursorLeft,
    CursorRight,
    CursorWordLeft,
    CursorWordRight,
    CursorToBegin,
    CursorToEnd,

    // History navigation
    HistoryPrevious,
    HistoryNext,
    HistorySearch,

    // Editing operations
    InsertChar(char),
    InsertPairedChar {
        open: char,
        close: char,
    },
    Backspace,
    DeleteCharForward,
    DeleteWordBackward,
    DeleteToEnd,
    DeleteToBeginning,
    /// Re-insert the text removed by the last kill (Ctrl-Y).
    Yank,
    Undo,
    Redo,

    // Completion / Suggestion
    TriggerCompletion,
    AcceptCompletion,
    AcceptSuggestionFull,
    AcceptSuggestionWord,
    RotateSuggestionForward,
    RotateSuggestionBackward,

    /// Insert the last argument of a previous command; repeat to walk further
    /// back through history (readline's `insert-last-argument`).
    InsertLastArgument,
    /// Pick a snippet and insert it at the cursor.
    InsertSnippet,
    /// Move to the next / previous `{{placeholder}}` of an inserted snippet.
    NextPlaceholder,
    PrevPlaceholder,

    // Command execution
    Execute,
    ExecuteBackground,
    /// Ctrl-D on an empty buffer: end of input, exit the shell.
    Eof,
    /// Ctrl-Z on an empty buffer: resume the most recently stopped job.
    ResumeLastJob,

    // Command Palette
    OpenCommandPalette,
    /// Browse this session's command blocks with their captured output.
    OpenBlockBrowser,

    // AI features
    AiAutoFix,
    AiSmartCommit,
    AiDiagnose,
    ForceAiSuggestion,
    AiExplainCommand,
    AiWatchCurrentInput,
    MacroRecord,

    // Others
    Paste,
    OpenEditor, // Open editor for current input
    ClearScreen,
    Interrupt,
    ToggleSudo,
    CancelCompletion,

    // Special handling (context dependent)
    OvertypeClosingBracket(char),
    ExpandAbbreviationAndInsertSpace,

    // Ignore
    Unsupported,
}

/// Context during key input (input to pure function)
#[derive(Debug, Clone, Default)]
pub struct KeyContext {
    /// Is cursor at the end of input
    pub cursor_at_end: bool,
    /// Is input empty
    pub input_empty: bool,
    /// Is suggestion active
    pub has_suggestion: bool,
    /// Is completion active (input.completion.is_some())
    pub has_completion: bool,
    /// Is in completion mode (completion.completion_mode())
    pub completion_mode: bool,
    /// Is cursor position 0
    pub cursor_at_start: bool,
    /// Next character (for OvertypeClosingBracket)
    pub next_char: Option<char>,
    /// Is auto-pair enabled
    pub auto_pair: bool,
    /// Is a multi-line continuation in progress (`multiline_buffer` non-empty).
    ///
    /// Ctrl-D must not exit the shell from an empty continuation line.
    pub multiline_active: bool,
}

/// Determine action from key input (pure function)
///
/// This function has no side effects and determines the action from key input and context.
/// For testability, it does not perform state changes.
pub fn determine_key_action(key: &KeyEvent, ctx: &KeyContext) -> KeyAction {
    match (key.code, key.modifiers) {
        // History navigation
        (KeyCode::Up, NONE) => KeyAction::HistoryPrevious,
        (KeyCode::Down, NONE) => KeyAction::HistoryNext,

        // Accept suggestion (Ctrl+Right, Alt+f by word)
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

        // Rotate suggestion
        (KeyCode::Char(']'), ALT) => KeyAction::RotateSuggestionForward,
        (KeyCode::Char('['), ALT) => KeyAction::RotateSuggestionBackward,

        // Cursor move (Left)
        (KeyCode::Left, m) if !m.contains(CTRL) => KeyAction::CursorLeft,
        (KeyCode::Left, m) if m.contains(CTRL) => KeyAction::CursorWordLeft,

        // Accept suggestion (Right for full)
        (KeyCode::Right, m)
            if ctx.has_suggestion
                && !ctx.has_completion
                && ctx.cursor_at_end
                && !m.contains(CTRL) =>
        {
            KeyAction::AcceptSuggestionFull
        }

        // Accept completion (ghost text) (Right)
        (KeyCode::Right, m) if ctx.has_completion && ctx.cursor_at_end && !m.contains(CTRL) => {
            KeyAction::AcceptCompletion
        }

        // Cursor move (Right)
        (KeyCode::Right, m) if !m.contains(CTRL) => KeyAction::CursorRight,
        (KeyCode::Right, m) if m.contains(CTRL) => KeyAction::CursorWordRight,

        // Ctrl+f to accept suggestion
        (KeyCode::Char('f'), CTRL)
            if ctx.has_suggestion && !ctx.has_completion && ctx.cursor_at_end =>
        {
            KeyAction::AcceptSuggestionFull
        }

        // Space input (Abbreviation expansion check)
        (KeyCode::Char(' '), NONE) => KeyAction::ExpandAbbreviationAndInsertSpace,

        // Auto-pairing: Open bracket
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

        // Overtype: Closing bracket
        (KeyCode::Char(ch), NONE) if ctx.auto_pair && matches!(ch, ')' | '}' | ']') => {
            if ctx.next_char == Some(ch) {
                KeyAction::OvertypeClosingBracket(ch)
            } else {
                KeyAction::InsertChar(ch)
            }
        }

        // Quote overtype
        (KeyCode::Char(ch), NONE) if ctx.auto_pair && matches!(ch, '\'' | '"') => {
            KeyAction::InsertChar(ch)
        }

        // Normal character input
        (KeyCode::Char(ch), NONE) => KeyAction::InsertChar(ch),
        (KeyCode::Char(ch), SHIFT) => KeyAction::InsertChar(ch),

        // Backspace / Delete
        (KeyCode::Backspace, NONE) => KeyAction::Backspace,
        (KeyCode::Delete, NONE) => KeyAction::DeleteCharForward,

        // Home / End
        (KeyCode::Home, NONE) => KeyAction::CursorToBegin,
        (KeyCode::End, NONE) => KeyAction::CursorToEnd,

        // AI features
        (KeyCode::Char('f'), ALT) => KeyAction::AiAutoFix,
        (KeyCode::Char('s'), ALT) => KeyAction::ForceAiSuggestion,
        (KeyCode::Char('e'), ALT) => KeyAction::AiExplainCommand,
        (KeyCode::Char('c'), ALT) => KeyAction::AiSmartCommit,
        (KeyCode::Char('d'), ALT) => KeyAction::AiDiagnose,
        (KeyCode::Char('w'), ALT) => KeyAction::AiWatchCurrentInput,
        (KeyCode::Char('m'), ALT) => KeyAction::MacroRecord,

        // Input shortcuts.
        //
        // `Alt+_` is readline's alias for `Alt+.`. Matching ALT exactly keeps it
        // clear of the `Ctrl+_` undo arm further down, which requires CTRL.
        (KeyCode::Char('.'), ALT) | (KeyCode::Char('_'), ALT) => KeyAction::InsertLastArgument,
        (KeyCode::Char(';'), ALT) => KeyAction::InsertSnippet,
        (KeyCode::Char('n'), ALT) => KeyAction::NextPlaceholder,
        (KeyCode::Char('p'), ALT) => KeyAction::PrevPlaceholder,

        // Tab: Completion
        (KeyCode::Tab, NONE) | (KeyCode::BackTab, NONE) => KeyAction::TriggerCompletion,

        // Enter: Execute
        (KeyCode::Enter, NONE) => KeyAction::Execute,
        (KeyCode::Enter, ALT) => KeyAction::ExecuteBackground,

        // Alt+x: Command Palette
        (KeyCode::Char('x'), ALT) => KeyAction::OpenCommandPalette,

        // Move to line start/end
        (KeyCode::Char('a'), CTRL) => KeyAction::CursorToBegin,
        // Ctrl+E: Accept completion if any, otherwise move to end of line
        (KeyCode::Char('e'), CTRL) if ctx.has_completion => KeyAction::AcceptCompletion,
        (KeyCode::Char('e'), CTRL) => KeyAction::CursorToEnd,

        // Ctrl+C: Interrupt
        (KeyCode::Char('c'), CTRL) => KeyAction::Interrupt,

        // Ctrl+L: Clear screen
        (KeyCode::Char('l'), CTRL) => KeyAction::ClearScreen,

        // Ctrl+D: EOF on an empty line, delete-forward otherwise (bash behavior).
        // A continuation line is never EOF: the command is still being typed.
        (KeyCode::Char('d'), CTRL) if ctx.input_empty && !ctx.multiline_active => KeyAction::Eof,
        (KeyCode::Char('d'), CTRL) => KeyAction::DeleteCharForward,

        // Ctrl+Z: resume the most recently stopped job (zsh ctrl-z style).
        // Suspending dsh itself would be wrong — it may be a login shell, which
        // is why SIGTSTP is deliberately ignored in `Shell::set_signals`.
        (KeyCode::Char('z'), CTRL) if ctx.input_empty => KeyAction::ResumeLastJob,

        // Ctrl+R: History search
        (KeyCode::Char('r'), CTRL) => KeyAction::HistorySearch,

        // Ctrl+O: Block browser
        (KeyCode::Char('o'), CTRL) => KeyAction::OpenBlockBrowser,

        // Ctrl+V: Paste
        (KeyCode::Char('v'), CTRL) => KeyAction::Paste,

        // Ctrl+W: Delete word
        (KeyCode::Char('w'), CTRL) => KeyAction::DeleteWordBackward,

        // Ctrl+K: Delete to end of line
        (KeyCode::Char('k'), CTRL) => KeyAction::DeleteToEnd,

        // Ctrl+U: Delete to beginning of line
        (KeyCode::Char('u'), CTRL) => KeyAction::DeleteToBeginning,

        // Ctrl+Y: Yank the last kill back in
        (KeyCode::Char('y'), CTRL) => KeyAction::Yank,

        // Ctrl+_ / Ctrl+/: Undo (the readline binding).
        //
        // These all arrive as the same 0x1F byte, which crossterm decodes as
        // `Ctrl+7` (`c - 0x1C + b'4'`). Terminals speaking the kitty keyboard
        // protocol instead report the literal key, and some add SHIFT for the
        // underscore, so accept every spelling.
        (KeyCode::Char('7'), CTRL) => KeyAction::Undo,
        (KeyCode::Char('_'), m) | (KeyCode::Char('/'), m) if m.contains(CTRL) => KeyAction::Undo,

        // Alt+/: Redo
        (KeyCode::Char('/'), ALT) => KeyAction::Redo,

        // Esc: Cancel completion or toggle sudo
        (KeyCode::Esc, NONE) => {
            if ctx.has_completion || ctx.has_suggestion {
                KeyAction::CancelCompletion
            } else {
                KeyAction::ToggleSudo
            }
        }

        // Others
        _ => KeyAction::Unsupported,
    }
}

#[cfg(test)]
mod tests;
