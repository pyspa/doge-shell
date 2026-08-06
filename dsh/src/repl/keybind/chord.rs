//! Parsing and normalisation of key specifications such as `"ctrl-g"`,
//! `"alt-."` and `"ctrl-x ctrl-e"`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt;

/// A single normalised keypress.
///
/// Normalisation happens once, at construction, so binding lookup is a plain
/// hash-map hit: see [`KeyStroke::from_event`] for the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct KeyStroke {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

/// One or more strokes that have to arrive in order.
pub(crate) type Chord = Vec<KeyStroke>;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChordParseError {
    Empty,
    /// Includes an unrecognised modifier prefix: prefix peeling stops at the
    /// first segment it does not know, so the remainder is reported as the key.
    UnknownKey(String),
}

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty key specification"),
            Self::UnknownKey(k) => write!(f, "unknown key '{k}'"),
        }
    }
}

impl KeyStroke {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        Self { code, mods }.normalized()
    }

    pub fn from_event(event: &KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    /// Collapses the spellings a terminal may produce for the same physical
    /// key so that binding and lookup agree.
    ///
    /// - SHIFT is dropped for character keys: the character already encodes it
    ///   (`shift-a` arrives as `A`), and keeping the flag would make `bind`
    ///   depend on how the terminal reports modifiers.
    /// - Control characters are matched case-insensitively.
    /// - `Ctrl+_`, `Ctrl+/` and `Ctrl+7` all travel as the byte 0x1F. crossterm
    ///   decodes it as `Ctrl+7`; terminals speaking the kitty protocol report
    ///   the literal key instead. They are folded together — the same reasoning
    ///   the built-in undo binding already applies.
    fn normalized(mut self) -> Self {
        if let KeyCode::Char(ch) = self.code {
            self.mods.remove(KeyModifiers::SHIFT);
            if self.mods.contains(KeyModifiers::CONTROL) {
                let lowered = ch.to_ascii_lowercase();
                self.code = KeyCode::Char(match lowered {
                    '_' | '/' => '7',
                    other => other,
                });
            } else {
                self.code = KeyCode::Char(ch);
            }
        }
        self
    }
}

impl fmt::Display for KeyStroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "ctrl-")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "alt-")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "shift-")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "space"),
            KeyCode::Char(ch) => write!(f, "{ch}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            KeyCode::Enter => write!(f, "enter"),
            KeyCode::Tab => write!(f, "tab"),
            KeyCode::BackTab => write!(f, "shift-tab"),
            KeyCode::Esc => write!(f, "esc"),
            KeyCode::Backspace => write!(f, "backspace"),
            KeyCode::Delete => write!(f, "delete"),
            KeyCode::Insert => write!(f, "insert"),
            KeyCode::Home => write!(f, "home"),
            KeyCode::End => write!(f, "end"),
            KeyCode::PageUp => write!(f, "pageup"),
            KeyCode::PageDown => write!(f, "pagedown"),
            KeyCode::Up => write!(f, "up"),
            KeyCode::Down => write!(f, "down"),
            KeyCode::Left => write!(f, "left"),
            KeyCode::Right => write!(f, "right"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Renders a chord the way `bind` would accept it back.
pub(crate) fn describe(chord: &[KeyStroke]) -> String {
    chord
        .iter()
        .map(|stroke| stroke.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses `"ctrl-x ctrl-e"` into its strokes.
///
/// Modifiers may be separated with `-` or `+`, and both the long and short
/// spellings are accepted (`ctrl`/`c`, `alt`/`m`/`meta`, `shift`/`s`).
pub(crate) fn parse_chord(spec: &str) -> Result<Chord, ChordParseError> {
    let strokes: Result<Chord, ChordParseError> =
        spec.split_whitespace().map(parse_stroke).collect();
    let strokes = strokes?;
    if strokes.is_empty() {
        return Err(ChordParseError::Empty);
    }
    Ok(strokes)
}

fn parse_stroke(spec: &str) -> Result<KeyStroke, ChordParseError> {
    if spec.is_empty() {
        return Err(ChordParseError::Empty);
    }

    let mut mods = KeyModifiers::NONE;
    let mut rest = spec;

    // Peel modifier prefixes. The separator is part of the prefix, so a bare
    // `-` or `+` key name is still reachable as the final segment.
    while let Some(split) = rest
        .find(['-', '+'])
        .filter(|index| *index > 0 && *index + 1 < rest.len())
    {
        let (candidate, remainder) = rest.split_at(split);
        let flag = match candidate.to_ascii_lowercase().as_str() {
            "ctrl" | "c" => KeyModifiers::CONTROL,
            "alt" | "m" | "meta" => KeyModifiers::ALT,
            "shift" | "s" => KeyModifiers::SHIFT,
            "super" | "cmd" => KeyModifiers::SUPER,
            _ => break,
        };
        mods |= flag;
        rest = &remainder[1..];
    }

    let code = parse_key_code(rest, &mut mods)?;
    Ok(KeyStroke::new(code, mods))
}

fn parse_key_code(name: &str, mods: &mut KeyModifiers) -> Result<KeyCode, ChordParseError> {
    let lowered = name.to_ascii_lowercase();

    if let Some(digits) = lowered.strip_prefix('f')
        && !digits.is_empty()
        && let Ok(n) = digits.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Ok(KeyCode::F(n));
    }

    let code = match lowered.as_str() {
        "enter" | "return" | "ret" => KeyCode::Enter,
        "tab" => {
            // `shift-tab` is its own key code rather than Tab plus a flag.
            if mods.contains(KeyModifiers::SHIFT) {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            } else {
                KeyCode::Tab
            }
        }
        "backtab" => KeyCode::BackTab,
        "space" | "spc" => KeyCode::Char(' '),
        "esc" | "escape" => KeyCode::Esc,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                // A single character key: take it verbatim so `alt-.` and the
                // uppercase form of `shift-a` both work.
                (Some(ch), None) => {
                    if mods.contains(KeyModifiers::SHIFT) && ch.is_ascii_alphabetic() {
                        mods.remove(KeyModifiers::SHIFT);
                        KeyCode::Char(ch.to_ascii_uppercase())
                    } else {
                        KeyCode::Char(ch)
                    }
                }
                _ => return Err(ChordParseError::UnknownKey(name.to_string())),
            }
        }
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(spec: &str) -> KeyStroke {
        parse_chord(spec).unwrap()[0]
    }

    #[test]
    fn parses_plain_modifiers() {
        assert_eq!(
            stroke("ctrl-g"),
            KeyStroke::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            stroke("alt-."),
            KeyStroke::new(KeyCode::Char('.'), KeyModifiers::ALT)
        );
    }

    #[test]
    fn accepts_short_spellings_and_plus_separator() {
        assert_eq!(stroke("c-g"), stroke("ctrl-g"));
        assert_eq!(stroke("m-x"), stroke("alt-x"));
        assert_eq!(stroke("ctrl+g"), stroke("ctrl-g"));
        assert_eq!(stroke("CTRL-G"), stroke("ctrl-g"));
    }

    #[test]
    fn parses_named_and_function_keys() {
        assert_eq!(stroke("enter").code, KeyCode::Enter);
        assert_eq!(stroke("space").code, KeyCode::Char(' '));
        assert_eq!(stroke("pageup").code, KeyCode::PageUp);
        assert_eq!(stroke("f5").code, KeyCode::F(5));
        assert_eq!(stroke("esc").code, KeyCode::Esc);
    }

    #[test]
    fn shift_tab_is_its_own_key_code() {
        assert_eq!(stroke("shift-tab").code, KeyCode::BackTab);
        assert_eq!(stroke("shift-tab").mods, KeyModifiers::NONE);
        assert_eq!(stroke("backtab"), stroke("shift-tab"));
    }

    #[test]
    fn shift_on_a_letter_becomes_the_uppercase_char() {
        assert_eq!(
            stroke("shift-a"),
            KeyStroke::new(KeyCode::Char('A'), KeyModifiers::NONE)
        );
    }

    /// The same physical key reaches us with different spellings depending on
    /// the terminal; binding must not depend on which one we happened to get.
    #[test]
    fn control_underscore_slash_and_seven_fold_together() {
        assert_eq!(stroke("ctrl-_"), stroke("ctrl-/"));
        assert_eq!(stroke("ctrl-_"), stroke("ctrl-7"));
    }

    #[test]
    fn control_letters_are_case_insensitive() {
        assert_eq!(stroke("ctrl-G"), stroke("ctrl-g"));
    }

    #[test]
    fn shift_reported_alongside_a_char_is_ignored_on_lookup() {
        let reported = KeyStroke::new(
            KeyCode::Char('_'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(reported, stroke("ctrl-_"));
    }

    #[test]
    fn parses_multi_stroke_chords() {
        let chord = parse_chord("ctrl-x ctrl-e").unwrap();
        assert_eq!(chord.len(), 2);
        assert_eq!(chord[0], stroke("ctrl-x"));
        assert_eq!(chord[1], stroke("ctrl-e"));
    }

    #[test]
    fn a_bare_separator_is_still_a_key() {
        assert_eq!(stroke("-").code, KeyCode::Char('-'));
        assert_eq!(stroke("alt--").code, KeyCode::Char('-'));
        assert_eq!(stroke("alt--").mods, KeyModifiers::ALT);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_chord("").is_err());
        assert!(parse_chord("ctrl-nope").is_err());
        assert_eq!(
            parse_chord("wat"),
            Err(ChordParseError::UnknownKey("wat".to_string()))
        );
    }

    #[test]
    fn round_trips_through_display() {
        for spec in ["ctrl-g", "alt-.", "f5", "enter", "space", "shift-tab"] {
            let chord = parse_chord(spec).unwrap();
            let rendered = describe(&chord);
            assert_eq!(
                parse_chord(&rendered).unwrap(),
                chord,
                "{spec} rendered as {rendered}"
            );
        }
    }

    #[test]
    fn describes_multi_stroke_chords_with_spaces() {
        let chord = parse_chord("ctrl-x ctrl-e").unwrap();
        assert_eq!(describe(&chord), "ctrl-x ctrl-e");
    }
}
