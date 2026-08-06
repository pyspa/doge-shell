//! User-configurable key bindings.
//!
//! [`crate::repl::key_action::determine_key_action`] stays exactly as it was —
//! a pure function with a large body of tests. This module sits *in front* of
//! it: a keypress is looked up in the user's map first, and only falls through
//! to the built-in table when nothing matches.
//!
//! **A user binding wins unconditionally.** Many built-in bindings are
//! context-sensitive (`Right` accepts a suggestion when one is showing, `Esc`
//! toggles `sudo` only when no completion is open). Rebinding such a key
//! replaces all of that with the single action named. This matches `bindkey`
//! and fish's `bind`, and keeps the rule easy to reason about.

pub(crate) mod action_name;
pub(crate) mod chord;

use crate::repl::key_action::KeyAction;
use chord::{Chord, KeyStroke};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// What a chord is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoundAction {
    /// One of the shell's built-in actions.
    Action(KeyAction),
    /// A Lisp function, called by name with the current input and cursor.
    Lisp(String),
}

/// Outcome of looking a keypress up in the bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A binding matched.
    Bound(BoundAction),
    /// The keypress starts (or continues) a longer chord; wait for more.
    Pending,
    /// A chord was started but this continuation is not bound. The string is
    /// the sequence, for the log.
    ///
    /// Callers must treat this like [`Self::Fallthrough`] and still dispatch
    /// the key that ended the chord. Swallowing it would mean an accidental
    /// prefix keypress makes the following Enter not run the command.
    Unbound(String),
    /// No binding involved — use the built-in table.
    Fallthrough,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct KeyBindings {
    map: HashMap<Chord, BoundAction>,
}

impl KeyBindings {
    /// The bindings every session starts with.
    ///
    /// Only multi-stroke chords live here. Single strokes stay in
    /// `determine_key_action` so its tests keep covering them, and so an empty
    /// user config costs nothing at lookup time.
    pub fn with_defaults() -> Self {
        let mut bindings = Self::default();
        bindings.insert(
            vec![
                KeyStroke::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
                KeyStroke::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
            ],
            BoundAction::Action(KeyAction::OpenEditor),
        );
        bindings
    }

    pub fn insert(&mut self, chord: Chord, action: BoundAction) {
        self.map.insert(chord, action);
    }

    /// Removes a binding, reporting whether there was one.
    pub fn remove(&mut self, chord: &[KeyStroke]) -> bool {
        self.map.remove(chord).is_some()
    }

    /// All bindings as `(chord, action)`, sorted for stable output.
    pub fn entries(&self) -> Vec<(String, &BoundAction)> {
        let mut entries: Vec<(String, &BoundAction)> = self
            .map
            .iter()
            .map(|(chord, action)| (chord::describe(chord), action))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// True when some longer chord starts with `prefix`.
    fn has_prefix(&self, prefix: &[KeyStroke]) -> bool {
        self.map
            .keys()
            .any(|chord| chord.len() > prefix.len() && chord.starts_with(prefix))
    }

    /// Resolves `event`, given the strokes already collected by a chord in
    /// progress. `pending` is updated in place.
    pub fn resolve(&self, pending: &mut Chord, event: &KeyEvent) -> Resolved {
        let was_pending = !pending.is_empty();
        pending.push(KeyStroke::from_event(event));

        if let Some(action) = self.map.get(pending.as_slice()) {
            pending.clear();
            return Resolved::Bound(action.clone());
        }

        if self.has_prefix(pending) {
            return Resolved::Pending;
        }

        let sequence = chord::describe(pending);
        pending.clear();

        if was_pending {
            // Mid-chord dead end. Reporting it beats the old behaviour of
            // silently swallowing whatever followed Ctrl-x.
            Resolved::Unbound(sequence)
        } else {
            Resolved::Fallthrough
        }
    }
}

#[cfg(test)]
mod tests {
    use super::chord::parse_chord;
    use super::*;

    fn event(spec: &str) -> KeyEvent {
        let stroke = parse_chord(spec).unwrap()[0];
        KeyEvent::new(stroke.code, stroke.mods)
    }

    fn bind(bindings: &mut KeyBindings, spec: &str, action: KeyAction) {
        bindings.insert(parse_chord(spec).unwrap(), BoundAction::Action(action));
    }

    #[test]
    fn unbound_keys_fall_through_to_the_builtin_table() {
        let bindings = KeyBindings::default();
        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-a")),
            Resolved::Fallthrough
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn a_single_stroke_binding_matches() {
        let mut bindings = KeyBindings::default();
        bind(&mut bindings, "ctrl-g", KeyAction::CancelCompletion);

        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-g")),
            Resolved::Bound(BoundAction::Action(KeyAction::CancelCompletion))
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn a_chord_waits_for_its_continuation() {
        let bindings = KeyBindings::with_defaults();
        let mut pending = Vec::new();

        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-x")),
            Resolved::Pending
        );
        assert_eq!(pending.len(), 1);

        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-e")),
            Resolved::Bound(BoundAction::Action(KeyAction::OpenEditor))
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn an_unfinished_chord_reports_the_sequence_and_resets() {
        let bindings = KeyBindings::with_defaults();
        let mut pending = Vec::new();

        bindings.resolve(&mut pending, &event("ctrl-x"));
        assert_eq!(
            bindings.resolve(&mut pending, &event("q")),
            Resolved::Unbound("ctrl-x q".to_string())
        );
        assert!(pending.is_empty());
    }

    /// A user binding replaces the built-in behaviour outright, including the
    /// context-sensitive parts.
    #[test]
    fn user_bindings_take_precedence_over_the_builtin_table() {
        let mut bindings = KeyBindings::default();
        bind(&mut bindings, "right", KeyAction::CursorRight);

        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("right")),
            Resolved::Bound(BoundAction::Action(KeyAction::CursorRight))
        );
    }

    #[test]
    fn lisp_bindings_resolve_to_the_function_name() {
        let mut bindings = KeyBindings::default();
        bindings.insert(
            parse_chord("ctrl-t").unwrap(),
            BoundAction::Lisp("my-fn".to_string()),
        );

        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-t")),
            Resolved::Bound(BoundAction::Lisp("my-fn".to_string()))
        );
    }

    #[test]
    fn removing_a_binding_restores_fallthrough() {
        let mut bindings = KeyBindings::default();
        bind(&mut bindings, "ctrl-g", KeyAction::CancelCompletion);
        assert!(bindings.remove(&parse_chord("ctrl-g").unwrap()));
        assert!(!bindings.remove(&parse_chord("ctrl-g").unwrap()));

        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-g")),
            Resolved::Fallthrough
        );
    }

    /// Unbinding the default chord must let `Ctrl-x` reach the built-in table
    /// rather than leaving it stuck as a dead prefix.
    #[test]
    fn unbinding_the_editor_chord_frees_its_prefix() {
        let mut bindings = KeyBindings::with_defaults();
        assert!(bindings.remove(&parse_chord("ctrl-x ctrl-e").unwrap()));

        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-x")),
            Resolved::Fallthrough
        );
    }

    #[test]
    fn a_shorter_binding_wins_over_a_longer_prefix_match() {
        let mut bindings = KeyBindings::with_defaults();
        // Binding ctrl-x itself makes it stop being a prefix.
        bind(&mut bindings, "ctrl-x", KeyAction::ClearScreen);

        let mut pending = Vec::new();
        assert_eq!(
            bindings.resolve(&mut pending, &event("ctrl-x")),
            Resolved::Bound(BoundAction::Action(KeyAction::ClearScreen))
        );
    }

    #[test]
    fn entries_are_sorted_for_stable_listing() {
        let mut bindings = KeyBindings::default();
        bind(&mut bindings, "ctrl-z", KeyAction::ClearScreen);
        bind(&mut bindings, "alt-a", KeyAction::Undo);

        let names: Vec<String> = bindings
            .entries()
            .into_iter()
            .map(|(chord, _)| chord)
            .collect();
        assert_eq!(names, vec!["alt-a", "ctrl-z"]);
    }
}
