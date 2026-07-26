//! Turn captured command output into plain, renderable lines.
//!
//! `PtyMonitor` appends the *raw* bytes it proxies to the output observer
//! (`process::io`, `observer.append(Stdout, &String::from_utf8_lossy(data))`),
//! not the display-filtered form. So a block captured through a PTY is full of
//! CSI/OSC sequences and `\r` progress rewrites — `blocks show` leaks both
//! today. The escape-sequence state machine here mirrors `PtyDisplayBuffer` in
//! `process::io`, discarding what that one forwards.

/// Longest run of bytes treated as an unterminated escape sequence before
/// giving up and emitting it as text. Mirrors `MAX_PENDING_CONTROL_BYTES`.
const MAX_PENDING_CONTROL: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
    ControlString,
    ControlStringEscape,
}

/// Remove ANSI escape sequences, keeping the printable text.
///
/// An unterminated sequence longer than [`MAX_PENDING_CONTROL`] is treated as
/// ordinary text rather than swallowing the rest of the output.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut state = State::Ground;
    let mut pending = String::new();

    for ch in input.chars() {
        match state {
            State::Ground => {
                if ch == '\x1b' {
                    pending.push(ch);
                    state = State::Escape;
                } else {
                    out.push(ch);
                }
            }
            State::Escape => {
                pending.push(ch);
                match ch {
                    '[' => state = State::Csi,
                    ']' => state = State::Osc,
                    'P' | 'X' | '^' | '_' => state = State::ControlString,
                    _ => {
                        // A two-character escape; drop it whole.
                        pending.clear();
                        state = State::Ground;
                    }
                }
            }
            State::Csi => {
                pending.push(ch);
                // Final byte of a CSI sequence.
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    pending.clear();
                    state = State::Ground;
                }
            }
            State::Osc => {
                pending.push(ch);
                match ch {
                    '\x07' => {
                        pending.clear();
                        state = State::Ground;
                    }
                    '\x1b' => state = State::OscEscape,
                    _ => {}
                }
            }
            State::OscEscape => {
                pending.push(ch);
                match ch {
                    '\\' => {
                        pending.clear();
                        state = State::Ground;
                    }
                    '\x1b' => {}
                    _ => state = State::Osc,
                }
            }
            State::ControlString => {
                pending.push(ch);
                match ch {
                    '\x07' => {
                        pending.clear();
                        state = State::Ground;
                    }
                    '\x1b' => state = State::ControlStringEscape,
                    _ => {}
                }
            }
            State::ControlStringEscape => {
                pending.push(ch);
                match ch {
                    '\\' => {
                        pending.clear();
                        state = State::Ground;
                    }
                    '\x1b' => {}
                    _ => state = State::ControlString,
                }
            }
        }

        // Not a real escape sequence after all: keep the text instead of
        // discarding the remainder of the output.
        if pending.chars().count() >= MAX_PENDING_CONTROL {
            out.push_str(&pending);
            pending.clear();
            state = State::Ground;
        }
    }

    // Trailing partial sequence: the output was truncated mid-escape.
    if !pending.is_empty() && pending.chars().count() >= MAX_PENDING_CONTROL {
        out.push_str(&pending);
    }

    out
}

/// Apply the overwrite semantics of `\r` and `\x08` within a single line.
///
/// A progress bar redraws one row over and over; without this a `cargo build`
/// block renders as thousands of near-identical lines.
fn apply_overwrites(line: &str) -> String {
    if !line.contains('\r') && !line.contains('\x08') {
        return line.to_string();
    }

    let mut cells: Vec<char> = Vec::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        match ch {
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            _ => {
                if col < cells.len() {
                    cells[col] = ch;
                } else {
                    cells.push(ch);
                }
                col += 1;
            }
        }
    }
    cells.into_iter().collect()
}

/// Drop control characters that would corrupt the layout. Tabs survive because
/// they carry alignment.
fn drop_control_chars(line: &str) -> String {
    line.chars()
        .filter(|ch| *ch == '\t' || !ch.is_control())
        .collect()
}

/// Split captured output into the lines a terminal would have displayed.
///
/// Strips escape sequences, resolves `\r`/backspace overwrites, and removes the
/// trailing blank line that output ending in a newline would otherwise produce.
pub fn display_lines(output: &str) -> Vec<String> {
    if output.is_empty() {
        return Vec::new();
    }

    let stripped = strip_ansi(output);
    let mut lines: Vec<String> = stripped
        .split('\n')
        .map(|line| drop_control_chars(&apply_overwrites(line)))
        .collect();

    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines
}

/// Rough share of the text that was ANSI escapes.
///
/// Full-screen programs (`vim`, `htop`) leave a block that is almost entirely
/// cursor positioning; after stripping it is noise, so the browser collapses
/// those by default rather than pretending they are readable output.
pub fn ansi_density(output: &str) -> f32 {
    if output.is_empty() {
        return 0.0;
    }
    let total = output.chars().count();
    let kept = strip_ansi(output).chars().count();
    (total - kept) as f32 / total as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("\x1b[1;32mbold green\x1b[m!"), "bold green!");
        // Cursor positioning, the bulk of a full-screen program's output.
        assert_eq!(strip_ansi("\x1b[2J\x1b[Hhome"), "home");
    }

    #[test]
    fn strip_ansi_removes_osc_strings() {
        // BEL-terminated and ST-terminated forms.
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
        assert_eq!(strip_ansi("\x1b]7;file:///tmp\x1b\\text"), "text");
        // The shell's own integration markers.
        assert_eq!(strip_ansi("\x1b]133;A\x1b\\$ ls"), "$ ls");
    }

    #[test]
    fn strip_ansi_removes_two_char_escapes_and_control_strings() {
        assert_eq!(strip_ansi("\x1b=alt\x1b>"), "alt");
        assert_eq!(strip_ansi("\x1bPsome dcs\x1b\\kept"), "kept");
    }

    #[test]
    fn strip_ansi_preserves_utf8() {
        assert_eq!(strip_ansi("\x1b[32m日本語\x1b[0m"), "日本語");
        assert_eq!(strip_ansi("絵文字 🐕 と ✔"), "絵文字 🐕 と ✔");
    }

    #[test]
    fn strip_ansi_keeps_a_runaway_sequence_as_text() {
        // An unterminated CSI must not swallow everything after it.
        let runaway = format!("\x1b[{}", "9".repeat(MAX_PENDING_CONTROL + 10));
        let out = strip_ansi(&runaway);
        assert!(out.contains('9'));
    }

    #[test]
    fn strip_ansi_leaves_plain_text_untouched() {
        assert_eq!(
            strip_ansi("plain output\nsecond line"),
            "plain output\nsecond line"
        );
    }

    #[test]
    fn collapse_carriage_return_progress_keeps_the_last_state() {
        let progress = "10%\r55%\r100% done";
        assert_eq!(apply_overwrites(progress), "100% done");
    }

    #[test]
    fn carriage_return_overwrite_keeps_uncovered_tail() {
        // A shorter redraw leaves the tail of the longer text visible, which is
        // what the terminal actually shows.
        assert_eq!(apply_overwrites("abcdef\rXY"), "XYcdef");
    }

    #[test]
    fn backspace_overwrites_the_previous_column() {
        assert_eq!(apply_overwrites("ab\x08c"), "ac");
    }

    #[test]
    fn display_lines_splits_and_drops_the_trailing_blank() {
        assert_eq!(display_lines("one\ntwo\n"), vec!["one", "two"]);
        assert_eq!(display_lines("one\ntwo"), vec!["one", "two"]);
        // A deliberate blank line in the middle survives.
        assert_eq!(display_lines("one\n\ntwo\n"), vec!["one", "", "two"]);
    }

    #[test]
    fn display_lines_handles_crlf() {
        assert_eq!(display_lines("one\r\ntwo\r\n"), vec!["one", "two"]);
    }

    #[test]
    fn display_lines_collapses_a_progress_bar_to_one_line() {
        let build = "Compiling 1/3\rCompiling 2/3\rCompiling 3/3\nDone\n";
        assert_eq!(display_lines(build), vec!["Compiling 3/3", "Done"]);
    }

    #[test]
    fn display_lines_strips_escapes_and_control_chars() {
        assert_eq!(display_lines("\x1b[32mok\x1b[0m\x07\n"), vec!["ok"]);
        // Tabs carry alignment and must survive.
        assert_eq!(display_lines("a\tb\n"), vec!["a\tb"]);
    }

    #[test]
    fn display_lines_of_empty_output_is_empty() {
        assert!(display_lines("").is_empty());
    }

    #[test]
    fn ansi_density_separates_plain_output_from_full_screen_programs() {
        assert_eq!(ansi_density(""), 0.0);
        assert!(ansi_density("plain text with no escapes") < 0.05);

        // A vim-like redraw: mostly cursor positioning.
        let full_screen: String = (0..40)
            .map(|row| format!("\x1b[{};1H\x1b[K~", row))
            .collect();
        assert!(ansi_density(&full_screen) > 0.8);
    }
}
