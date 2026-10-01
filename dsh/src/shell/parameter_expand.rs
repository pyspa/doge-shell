//! Pure POSIX parameter-expansion semantics for named shell variables.
//!
//! This module owns the `set/non-empty`, `set/empty`, `unset` state matrix
//! and the `Default / Assign / Error / Alternate` × `UnsetOnly / UnsetOrNull`
//! decision function. It never spawns, splits, globs, or touches the
//! environment: `word_expand` orchestrates runtime expansion and applies the
//! assignment side effect only when the decision says `AssignWord`.

use super::plan::{ParameterAction, ParameterCondition};
use std::fmt;

/// Set/unset-preserving parameter state.
///
/// `Some("")` is set-but-null; `None` is unset. Never collapse them into a
/// bare `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParameterState {
    pub value: String,
    pub is_set: bool,
}

impl ParameterState {
    pub(crate) fn set(value: String) -> Self {
        Self {
            value,
            is_set: true,
        }
    }

    pub(crate) fn unset() -> Self {
        Self {
            value: String::new(),
            is_set: false,
        }
    }
}

/// What the state matrix selects for one expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParameterDecision {
    /// Substitute the current parameter value.
    UseParameter,
    /// Expand and substitute `word`.
    UseWord,
    /// Expand `word` scalar, assign it, then substitute the new value.
    AssignWord,
    /// Fatal expansion error (lazy diagnostic `word` when present).
    Error,
    /// Substitute null (zero fields unquoted, one empty field quoted).
    UseNull,
}

/// Pure state matrix:
///
/// ```text
/// UnsetOnly: match only !is_set
/// UnsetOrNull: match !is_set || value.is_empty()
///
/// Default:   matched -> UseWord,      not matched -> UseParameter
/// Assign:    matched -> AssignWord,   not matched -> UseParameter
/// Error:     matched -> Error,        not matched -> UseParameter
/// Alternate: matched -> UseNull,      not matched -> UseWord
/// ```
pub(crate) fn decide_parameter_expansion(
    state: &ParameterState,
    condition: ParameterCondition,
    action: ParameterAction,
) -> ParameterDecision {
    let matched = match condition {
        ParameterCondition::UnsetOnly => !state.is_set,
        ParameterCondition::UnsetOrNull => !state.is_set || state.value.is_empty(),
    };
    match (action, matched) {
        (ParameterAction::Default, true) => ParameterDecision::UseWord,
        (ParameterAction::Default, false) => ParameterDecision::UseParameter,
        (ParameterAction::Assign, true) => ParameterDecision::AssignWord,
        (ParameterAction::Assign, false) => ParameterDecision::UseParameter,
        (ParameterAction::Error, true) => ParameterDecision::Error,
        (ParameterAction::Error, false) => ParameterDecision::UseParameter,
        (ParameterAction::Alternate, true) => ParameterDecision::UseNull,
        (ParameterAction::Alternate, false) => ParameterDecision::UseWord,
    }
}

/// Fatal `${VAR:?word}` / `${VAR?word}` expansion failure.
///
/// A typed semantic error, never an ordinary command failure
/// (`MaterializeOutcome::Rejected`) and never an infrastructure `E` verdict.
/// Carries the parameter name plus the lazily expanded diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParameterExpansionError {
    pub parameter: String,
    pub message: String,
}

impl fmt::Display for ParameterExpansionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.parameter, self.message)
    }
}

impl std::error::Error for ParameterExpansionError {}

/// Whether `err` (possibly wrapped in `anyhow`) is a typed parameter
/// expansion failure. Never classifies by substring matching.
pub(crate) fn is_parameter_expansion_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ParameterExpansionError>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_non_empty() -> ParameterState {
        ParameterState::set("value".to_string())
    }

    fn set_empty() -> ParameterState {
        ParameterState::set(String::new())
    }

    fn unset() -> ParameterState {
        ParameterState::unset()
    }

    #[test]
    fn default_colon_matrix() {
        use ParameterAction::Default;
        let cond = ParameterCondition::UnsetOrNull;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Default),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Default),
            ParameterDecision::UseWord
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Default),
            ParameterDecision::UseWord
        );
    }

    #[test]
    fn default_unset_only_matrix() {
        use ParameterAction::Default;
        let cond = ParameterCondition::UnsetOnly;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Default),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Default),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Default),
            ParameterDecision::UseWord
        );
    }

    #[test]
    fn assign_colon_matrix() {
        use ParameterAction::Assign;
        let cond = ParameterCondition::UnsetOrNull;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Assign),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Assign),
            ParameterDecision::AssignWord
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Assign),
            ParameterDecision::AssignWord
        );
    }

    #[test]
    fn assign_unset_only_matrix() {
        use ParameterAction::Assign;
        let cond = ParameterCondition::UnsetOnly;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Assign),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Assign),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Assign),
            ParameterDecision::AssignWord
        );
    }

    #[test]
    fn error_colon_matrix() {
        use ParameterAction::Error;
        let cond = ParameterCondition::UnsetOrNull;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Error),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Error),
            ParameterDecision::Error
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Error),
            ParameterDecision::Error
        );
    }

    #[test]
    fn error_unset_only_matrix() {
        use ParameterAction::Error;
        let cond = ParameterCondition::UnsetOnly;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Error),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Error),
            ParameterDecision::UseParameter
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Error),
            ParameterDecision::Error
        );
    }

    #[test]
    fn alternate_colon_matrix() {
        use ParameterAction::Alternate;
        let cond = ParameterCondition::UnsetOrNull;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Alternate),
            ParameterDecision::UseWord
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Alternate),
            ParameterDecision::UseNull
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Alternate),
            ParameterDecision::UseNull
        );
    }

    #[test]
    fn alternate_unset_only_matrix() {
        use ParameterAction::Alternate;
        let cond = ParameterCondition::UnsetOnly;
        assert_eq!(
            decide_parameter_expansion(&set_non_empty(), cond, Alternate),
            ParameterDecision::UseWord
        );
        assert_eq!(
            decide_parameter_expansion(&set_empty(), cond, Alternate),
            ParameterDecision::UseWord
        );
        assert_eq!(
            decide_parameter_expansion(&unset(), cond, Alternate),
            ParameterDecision::UseNull
        );
    }

    #[test]
    fn error_displays_parameter_and_message() {
        let err = ParameterExpansionError {
            parameter: "X".to_string(),
            message: "boom".to_string(),
        };
        assert_eq!(format!("{err}"), "X: boom");
        let wrapped = anyhow::anyhow!(err.clone());
        assert!(is_parameter_expansion_error(&wrapped));
        assert!(!is_parameter_expansion_error(&anyhow::anyhow!("X: boom")));
    }
}
