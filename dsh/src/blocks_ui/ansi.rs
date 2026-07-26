//! Turn captured command output into plain, renderable lines.
//!
//! `PtyMonitor` appends the *raw* bytes it proxies to the output observer
//! (`process::io`, `observer.append(Stdout, &String::from_utf8_lossy(data))`),
//! not the display-filtered form. So a block captured through a PTY is full of
//! CSI/OSC sequences and `\r` progress rewrites — `blocks show` leaks both
//! today. The escape-sequence state machine here mirrors `PtyDisplayBuffer` in
//! `process::io`, discarding what that one forwards.

/// Longest run treated as an unterminated escape sequence before giving up and
/// emitting it as text. Matches `MAX_PENDING_CONTROL_BYTES` in `process::io`.
///
/// It has to be this generous: OSC payloads are routinely long. A cwd
/// notification carrying a deep path (`\x1b]7;file://host/very/long/path\x1b\`)
/// runs past a hundred characters, and hyperlinks and clipboard writes go
/// further. Cutting the sequence short does not merely fail to strip it — the
/// escape bytes get emitted into the rendered output as visible garbage.
const MAX_PENDING_CONTROL: usize = 4096;

/// The escape sequence being consumed, with its character count maintained
/// alongside it.
///
/// Recomputing the count per character would be quadratic in the sequence
/// length, which matters once [`MAX_PENDING_CONTROL`] allows sequences of a few
/// thousand characters. Keeping the two together means the count cannot drift
/// from the buffer.
#[derive(Default)]
struct Pending {
    text: String,
    chars: usize,
}

impl Pending {
    fn push(&mut self, ch: char) {
        self.text.push(ch);
        self.chars += 1;
    }

    fn clear(&mut self) {
        self.text.clear();
        self.chars = 0;
    }

    fn is_overlong(&self) -> bool {
        self.chars >= MAX_PENDING_CONTROL
    }
}

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
    scan(input, |ch| out.push(ch));
    out
}

/// Drive the escape-sequence state machine, handing every kept character to
/// `keep`.
///
/// Callers that only need a count must not have to materialise the stripped
/// string: doing that for every block up front made opening the browser cost
/// seconds at the 100-block × 1 MiB cap.
fn scan(input: &str, mut keep: impl FnMut(char)) {
    let mut state = State::Ground;
    let mut pending = Pending::default();

    for ch in input.chars() {
        match state {
            State::Ground => {
                if ch == '\x1b' {
                    pending.push(ch);
                    state = State::Escape;
                } else {
                    keep(ch);
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
        if pending.is_overlong() {
            for pending_ch in pending.text.chars() {
                keep(pending_ch);
            }
            pending.clear();
            state = State::Ground;
        }
    }

    // Anything still pending is a sequence the output was truncated in the
    // middle of; it is shorter than MAX_PENDING_CONTROL (the check above
    // flushes past that) and is escape bytes, so drop it.
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

/// Bytes of each block sampled to classify it. Density is a heuristic, and the
/// browser classifies every block up front, so the cost must not scale with the
/// 1 MiB capture cap.
const DENSITY_SAMPLE_BYTES: usize = 64 * 1024;

/// Rough share of the text that was ANSI escapes.
///
/// Full-screen programs (`vim`, `htop`) leave a block that is almost entirely
/// cursor positioning; after stripping it is noise, so the browser collapses
/// those by default rather than pretending they are readable output.
///
/// Counts through [`scan`] instead of measuring `strip_ansi`'s output: building
/// a stripped copy of every block just to compare lengths is what made opening
/// the browser cost a visible pause.
pub fn ansi_density(output: &str) -> f32 {
    let sample = sample_prefix(output, DENSITY_SAMPLE_BYTES);
    if sample.is_empty() {
        return 0.0;
    }

    let total = sample.chars().count();
    let mut kept = 0usize;
    scan(sample, |_| kept += 1);
    (total - kept) as f32 / total as f32
}

/// Longest prefix of `text` that is at most `max_bytes` long and ends on a
/// character boundary.
fn sample_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
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
    fn strip_ansi_removes_a_long_osc_payload() {
        // Regression: with a small pending cap, a realistic cwd notification
        // was flushed as text and its URL showed up in the rendered output.
        let osc7 = "\x1b]7;file://myhost/tmp/claude-1000/-home-ma2-repos-github-com-pyspa-doge-shell/94e5db58-d4cd-4d56-8266-26cb9ea82983/scratchpad\x1b\\";
        assert!(osc7.chars().count() > 100);
        assert_eq!(strip_ansi(&format!("{osc7}real output")), "real output");

        // OSC 8 hyperlinks wrap the visible text and are longer still.
        let link = format!(
            "\x1b]8;;https://example.com/{}\x07label\x1b]8;;\x07",
            "segment/".repeat(40)
        );
        assert_eq!(strip_ansi(&link), "label");
    }

    #[test]
    fn strip_ansi_removes_a_long_multibyte_osc_title() {
        let title = format!("\x1b]0;{}\x07after", "日本語のタイトル".repeat(30));
        assert_eq!(strip_ansi(&title), "after");
    }

    #[test]
    fn pending_keeps_its_count_in_sync_with_its_text() {
        // The count exists to avoid a quadratic recount; if it ever drifts from
        // the buffer the overlong check fires at the wrong point.
        let mut pending = Pending::default();
        assert_eq!(pending.chars, 0);
        assert!(!pending.is_overlong());

        for ch in "\x1b]0;日本".chars() {
            pending.push(ch);
        }
        assert_eq!(pending.chars, pending.text.chars().count());
        // Multi-byte content: the count is characters, not bytes.
        assert!(pending.text.len() > pending.chars);

        pending.clear();
        assert_eq!(pending.chars, 0);
        assert!(pending.text.is_empty());
    }

    #[test]
    fn strip_ansi_handles_a_large_run_of_escape_garbage() {
        // Quadratic recounting of the pending buffer would make this crawl.
        let garbage = "\x1b[".repeat(200_000);
        let out = strip_ansi(&format!("{garbage}tail"));
        assert!(out.ends_with("tail"));
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
    fn sample_prefix_never_splits_a_character() {
        let text = "あいうえお"; // 3 bytes each
        // 7 is mid-character; the prefix must back off to 6.
        let sample = sample_prefix(text, 7);
        assert_eq!(sample, "あい");
        assert!(text.starts_with(sample));

        // Shorter than the cap: returned whole.
        assert_eq!(sample_prefix(text, 1000), text);
        // Cap smaller than the first character: empty rather than a panic.
        assert_eq!(sample_prefix(text, 2), "");
        assert_eq!(sample_prefix("", 10), "");
    }

    #[test]
    fn ansi_density_only_samples_a_prefix() {
        // Escapes up front, then far more plain text than the sample window:
        // the verdict comes from the sample, not the whole block.
        let noisy_head: String = "\x1b[1;1H\x1b[K".repeat(DENSITY_SAMPLE_BYTES / 9);
        let long_tail = "plain text\n".repeat(200_000);
        let block = format!("{noisy_head}{long_tail}");
        assert!(block.len() > DENSITY_SAMPLE_BYTES * 4);
        assert!(ansi_density(&block) > 0.5);

        // And a block whose sampled head is plain stays plain.
        let plain_head = format!("{long_tail}{noisy_head}");
        assert!(ansi_density(&plain_head) < 0.5);
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
