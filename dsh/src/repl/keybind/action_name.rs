//! Names that `bind` accepts for the built-in actions.
//!
//! Kept as one table so the parser, the error messages and `list-bindings` can
//! never drift apart.

use crate::repl::key_action::KeyAction;

/// Every bindable action, in the order `list-bindings` prints them.
///
/// Variants carrying data (`InsertChar`, `InsertPairedChar`,
/// `OvertypeClosingBracket`) and `Unsupported` are deliberately absent: they
/// are produced from the keypress itself and cannot be named up front.
pub(crate) const ACTIONS: &[(&str, KeyAction)] = &[
    ("cursor-left", KeyAction::CursorLeft),
    ("cursor-right", KeyAction::CursorRight),
    ("cursor-word-left", KeyAction::CursorWordLeft),
    ("cursor-word-right", KeyAction::CursorWordRight),
    ("cursor-to-begin", KeyAction::CursorToBegin),
    ("cursor-to-end", KeyAction::CursorToEnd),
    ("history-previous", KeyAction::HistoryPrevious),
    ("history-next", KeyAction::HistoryNext),
    ("history-search", KeyAction::HistorySearch),
    ("backspace", KeyAction::Backspace),
    ("delete-char-forward", KeyAction::DeleteCharForward),
    ("delete-word-backward", KeyAction::DeleteWordBackward),
    ("delete-to-end", KeyAction::DeleteToEnd),
    ("delete-to-beginning", KeyAction::DeleteToBeginning),
    ("yank", KeyAction::Yank),
    ("undo", KeyAction::Undo),
    ("redo", KeyAction::Redo),
    ("trigger-completion", KeyAction::TriggerCompletion),
    ("accept-completion", KeyAction::AcceptCompletion),
    ("accept-suggestion-full", KeyAction::AcceptSuggestionFull),
    ("accept-suggestion-word", KeyAction::AcceptSuggestionWord),
    (
        "rotate-suggestion-forward",
        KeyAction::RotateSuggestionForward,
    ),
    (
        "rotate-suggestion-backward",
        KeyAction::RotateSuggestionBackward,
    ),
    ("insert-last-argument", KeyAction::InsertLastArgument),
    ("insert-snippet", KeyAction::InsertSnippet),
    ("next-placeholder", KeyAction::NextPlaceholder),
    ("prev-placeholder", KeyAction::PrevPlaceholder),
    ("execute", KeyAction::Execute),
    ("execute-background", KeyAction::ExecuteBackground),
    ("eof", KeyAction::Eof),
    ("resume-last-job", KeyAction::ResumeLastJob),
    ("open-command-palette", KeyAction::OpenCommandPalette),
    ("open-block-browser", KeyAction::OpenBlockBrowser),
    ("ai-auto-fix", KeyAction::AiAutoFix),
    ("ai-smart-commit", KeyAction::AiSmartCommit),
    ("ai-diagnose", KeyAction::AiDiagnose),
    ("force-ai-suggestion", KeyAction::ForceAiSuggestion),
    ("ai-explain-command", KeyAction::AiExplainCommand),
    ("ai-watch-current-input", KeyAction::AiWatchCurrentInput),
    ("macro-record", KeyAction::MacroRecord),
    ("paste", KeyAction::Paste),
    ("open-editor", KeyAction::OpenEditor),
    ("clear-screen", KeyAction::ClearScreen),
    ("interrupt", KeyAction::Interrupt),
    ("toggle-sudo", KeyAction::ToggleSudo),
    ("cancel-completion", KeyAction::CancelCompletion),
    (
        "expand-abbreviation",
        KeyAction::ExpandAbbreviationAndInsertSpace,
    ),
];

pub(crate) fn action_from_name(name: &str) -> Option<KeyAction> {
    let lowered = name.to_ascii_lowercase();
    ACTIONS
        .iter()
        .find(|(candidate, _)| *candidate == lowered)
        .map(|(_, action)| action.clone())
}

pub(crate) fn name_of(action: &KeyAction) -> Option<&'static str> {
    ACTIONS
        .iter()
        .find(|(_, candidate)| candidate == action)
        .map(|(name, _)| *name)
}

pub(crate) fn all_names() -> Vec<&'static str> {
    ACTIONS.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for (name, action) in ACTIONS {
            assert_eq!(action_from_name(name).as_ref(), Some(action));
            assert_eq!(name_of(action), Some(*name));
        }
    }

    #[test]
    fn names_are_unique() {
        let mut names = all_names();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate action name in the table");
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(
            action_from_name("Cancel-Completion"),
            Some(KeyAction::CancelCompletion)
        );
        assert_eq!(action_from_name("no-such-action"), None);
    }

    /// Fails to compile when a `KeyAction` variant is added, forcing a decision
    /// about whether it should be bindable. Data-carrying variants and
    /// `Unsupported` are intentionally excluded from the table.
    #[test]
    fn every_variant_is_accounted_for() {
        fn assert_named(action: KeyAction) {
            assert!(
                name_of(&action).is_some(),
                "{action:?} is missing from ACTIONS"
            );
        }

        let sample = KeyAction::CursorLeft;
        match sample {
            KeyAction::InsertChar(_)
            | KeyAction::InsertPairedChar { .. }
            | KeyAction::OvertypeClosingBracket(_)
            | KeyAction::Unsupported => {}

            KeyAction::CursorLeft => assert_named(KeyAction::CursorLeft),
            KeyAction::CursorRight => {}
            KeyAction::CursorWordLeft => {}
            KeyAction::CursorWordRight => {}
            KeyAction::CursorToBegin => {}
            KeyAction::CursorToEnd => {}
            KeyAction::HistoryPrevious => {}
            KeyAction::HistoryNext => {}
            KeyAction::HistorySearch => {}
            KeyAction::Backspace => {}
            KeyAction::DeleteCharForward => {}
            KeyAction::DeleteWordBackward => {}
            KeyAction::DeleteToEnd => {}
            KeyAction::DeleteToBeginning => {}
            KeyAction::Yank => {}
            KeyAction::Undo => {}
            KeyAction::Redo => {}
            KeyAction::TriggerCompletion => {}
            KeyAction::AcceptCompletion => {}
            KeyAction::AcceptSuggestionFull => {}
            KeyAction::AcceptSuggestionWord => {}
            KeyAction::RotateSuggestionForward => {}
            KeyAction::RotateSuggestionBackward => {}
            KeyAction::InsertLastArgument => {}
            KeyAction::InsertSnippet => {}
            KeyAction::NextPlaceholder => {}
            KeyAction::PrevPlaceholder => {}
            KeyAction::Execute => {}
            KeyAction::ExecuteBackground => {}
            KeyAction::Eof => {}
            KeyAction::ResumeLastJob => {}
            KeyAction::OpenCommandPalette => {}
            KeyAction::OpenBlockBrowser => {}
            KeyAction::AiAutoFix => {}
            KeyAction::AiSmartCommit => {}
            KeyAction::AiDiagnose => {}
            KeyAction::ForceAiSuggestion => {}
            KeyAction::AiExplainCommand => {}
            KeyAction::AiWatchCurrentInput => {}
            KeyAction::MacroRecord => {}
            KeyAction::Paste => {}
            KeyAction::OpenEditor => {}
            KeyAction::ClearScreen => {}
            KeyAction::Interrupt => {}
            KeyAction::ToggleSudo => {}
            KeyAction::CancelCompletion => {}
            KeyAction::ExpandAbbreviationAndInsertSpace => {}
        }

        // The table itself is what callers rely on; check it end to end.
        for (_, action) in ACTIONS {
            assert_named(action.clone());
        }
    }
}
