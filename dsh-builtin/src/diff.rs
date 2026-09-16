//! What a write would change, for a person about to approve it.
//!
//! [`unified_lines`] is the line diff `skill diff` has always printed;
//! [`preview`] is the same diff bounded and coloured for an approval prompt,
//! where the question is "is this the change you meant?" and the answer has to
//! fit on a screen next to it.

/// Above this many lines on either side, the LCS table below (`O(n*m)` `u32`
/// cells) stops being a review command's problem to allocate. A file this
/// large was never going to be reviewed line by line anyway.
pub(crate) const MAX_DIFF_LINES: usize = 2000;

/// A minimal line diff: shared lines once, `old`-only lines prefixed `-`,
/// `new`-only lines prefixed `+`. Not a real diff algorithm - a proposal is
/// SKILL.md-sized, and pulling in a dependency for this would cost more than
/// it saves.
///
/// Falls back to showing the new content whole past `MAX_DIFF_LINES` on
/// either side: the target on disk has no size cap of its own (unlike a
/// staged proposal's body, which `skill_manage`'s lint already bounds), so
/// without this an unusually large file made the O(n*m) table below - not
/// just this function's input - the thing that could hang or OOM the shell.
pub(crate) fn unified_lines(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let (n, m) = (old_lines.len(), new_lines.len());

    if n > MAX_DIFF_LINES || m > MAX_DIFF_LINES {
        return format!(
            "(too large to diff line by line: {n} existing lines vs {m} proposed lines; showing the proposed content in full)\n\n{new}"
        );
    }

    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = String::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            out.push_str("  ");
            out.push_str(old_lines[i]);
            out.push('\n');
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push_str("- ");
            out.push_str(old_lines[i]);
            out.push('\n');
            i += 1;
        } else {
            out.push_str("+ ");
            out.push_str(new_lines[j]);
            out.push('\n');
            j += 1;
        }
    }
    for line in &old_lines[i..] {
        out.push_str("- ");
        out.push_str(line);
        out.push('\n');
    }
    for line in &new_lines[j..] {
        out.push_str("+ ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Lines of unchanged context kept either side of a change.
const CONTEXT_LINES: usize = 2;
/// Most lines one preview may print, however many the change touches.
const MAX_PREVIEW_LINES: usize = 40;
/// Longest single line a preview shows. A minified bundle is one line.
const MAX_PREVIEW_COLUMNS: usize = 200;

/// What a write would do, ready to show and to name.
pub(crate) struct Change {
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// Coloured, bounded, secrets redacted. Empty when nothing changed.
    pub(crate) body: String,
}

impl Change {
    /// The half that goes in the question, e.g. `+3 -1`.
    pub(crate) fn summary(&self) -> String {
        match (self.added, self.removed) {
            (0, 0) => "no change".to_string(),
            (added, 0) => format!("+{added}"),
            (0, removed) => format!("-{removed}"),
            (added, removed) => format!("+{added} -{removed}"),
        }
    }
}

/// What `old` becoming `new` looks like.
///
/// `old` is `None` for a file that does not exist yet, which is the common
/// `edit` case and reads better as "new file" than as a diff against nothing.
///
/// The approval prompt is the last point where a person can say no, and until
/// this existed it named the file and nothing else - so the rational answer to
/// "AI wants to write to file: x" was to stop reading and press `a`. Everything
/// here serves being *readable at a glance*: unchanged runs collapse, long
/// lines are cut, and the whole thing stops at [`MAX_PREVIEW_LINES`].
pub(crate) fn preview(old: Option<&str>, new: &str) -> Change {
    let Some(old) = old else {
        let lines: Vec<&str> = new.lines().collect();
        return Change {
            added: lines.len(),
            removed: 0,
            body: render(
                lines
                    .iter()
                    .map(|line| ('+', (*line).to_string()))
                    .collect(),
                "new file",
            ),
        };
    };

    let diff = unified_lines(old, new);
    let mut marked: Vec<(char, &str)> = Vec::new();
    let mut added = 0;
    let mut removed = 0;

    for line in diff.lines() {
        // `unified_lines` prefixes every line with two characters; the
        // fallback it returns for an oversized input does not, and is shown
        // as plain context rather than mislabelled as a change.
        let (marker, text) = match line.split_at_checked(2) {
            Some(("+ ", text)) => ('+', text),
            Some(("- ", text)) => ('-', text),
            Some(("  ", text)) => (' ', text),
            _ => (' ', line),
        };
        match marker {
            '+' => added += 1,
            '-' => removed += 1,
            _ => {}
        }
        marked.push((marker, text));
    }

    Change {
        added,
        removed,
        body: render(collapse_context(marked), ""),
    }
}

/// Drop the unchanged lines nobody needs to see.
///
/// A one-line edit to a thousand-line file is a thousand lines of context and
/// one of signal; keeping [`CONTEXT_LINES`] either side of each change is what
/// makes the signal findable.
fn collapse_context(lines: Vec<(char, &str)>) -> Vec<(char, String)> {
    let changed: Vec<bool> = lines.iter().map(|(marker, _)| *marker != ' ').collect();
    let keep: Vec<bool> = (0..lines.len())
        .map(|index| {
            let low = index.saturating_sub(CONTEXT_LINES);
            let high = (index + CONTEXT_LINES).min(lines.len().saturating_sub(1));
            changed[low..=high].iter().any(|changed| *changed)
        })
        .collect();

    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (index, line) in lines.into_iter().enumerate() {
        if keep[index] {
            if skipped > 0 {
                out.push(('~', format!("{skipped} unchanged line(s)")));
                skipped = 0;
            }
            out.push((line.0, line.1.to_string()));
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        out.push(('~', format!("{skipped} unchanged line(s)")));
    }
    out
}

fn render(lines: Vec<(char, String)>, note: &str) -> String {
    let mut out = String::new();
    if !note.is_empty() {
        out.push_str(&format!("\x1b[2m({note})\x1b[0m\n"));
    }

    let shown = lines.len().min(MAX_PREVIEW_LINES);
    for (marker, text) in lines.iter().take(shown) {
        let text = clip(text);
        let text = dsh_types::safety_policy::redact_sensitive_text(&text);
        match marker {
            '+' => out.push_str(&format!("\x1b[32m+ {text}\x1b[0m\n")),
            '-' => out.push_str(&format!("\x1b[31m- {text}\x1b[0m\n")),
            '~' => out.push_str(&format!("\x1b[2m  ⋮ {text}\x1b[0m\n")),
            _ => out.push_str(&format!("\x1b[2m  {text}\x1b[0m\n")),
        }
    }
    if lines.len() > shown {
        out.push_str(&format!(
            "\x1b[2m  … {} more line(s); the whole change is still written if you allow it\x1b[0m\n",
            lines.len() - shown
        ));
    }
    out
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_PREVIEW_COLUMNS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_PREVIEW_COLUMNS).collect();
    format!("{kept} …")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_lines_diffs_a_small_change_line_by_line() {
        let diff = unified_lines("a\nb\nc\n", "a\nx\nc\n");
        assert!(diff.contains("- b"), "{diff}");
        assert!(diff.contains("+ x"), "{diff}");
        assert!(diff.contains("  a"), "{diff}");
    }

    /// An on-disk file has no size cap the way a staged proposal's body
    /// does, so an unusually large one must fall back rather than build an
    /// `O(n*m)` table sized by it.
    #[test]
    fn unified_lines_falls_back_instead_of_building_an_unbounded_table() {
        let huge = "line\n".repeat(MAX_DIFF_LINES + 1);
        let diff = unified_lines(&huge, "new content\n");
        assert!(diff.contains("too large to diff"), "{diff}");
        assert!(diff.contains("new content"), "{diff}");
    }

    /// The question used to name the file and nothing else, so the rational
    /// answer to a run of them was to stop reading and press "always".
    #[test]
    fn a_preview_names_what_changed_on_both_sides() {
        let change = preview(Some("a\nb\nc\n"), "a\nx\nc\n");

        assert_eq!((change.added, change.removed), (1, 1));
        assert_eq!(change.summary(), "+1 -1");
        assert!(change.body.contains("- b"), "{}", change.body);
        assert!(change.body.contains("+ x"), "{}", change.body);
    }

    /// A file that does not exist yet reads better as "new file" than as a
    /// diff against nothing.
    #[test]
    fn a_new_file_is_shown_as_one() {
        let change = preview(None, "hello\nworld\n");

        assert_eq!((change.added, change.removed), (2, 0));
        assert_eq!(change.summary(), "+2");
        assert!(change.body.contains("new file"), "{}", change.body);
        assert!(change.body.contains("+ hello"), "{}", change.body);
    }

    /// A one-line edit to a long file is one line of signal and hundreds of
    /// context; the prompt has to stay readable at a glance.
    #[test]
    fn unchanged_runs_collapse_instead_of_filling_the_screen() {
        let old = format!("{}target\n{}", "keep\n".repeat(50), "keep\n".repeat(50));
        let new = old.replace("target", "changed");

        let change = preview(Some(&old), &new);

        assert_eq!((change.added, change.removed), (1, 1));
        assert!(change.body.contains("unchanged line(s)"), "{}", change.body);
        assert!(
            change.body.lines().count() <= MAX_PREVIEW_LINES + 1,
            "preview ran to {} lines",
            change.body.lines().count()
        );
        // The change itself survives the collapsing.
        assert!(change.body.contains("+ changed"), "{}", change.body);
    }

    /// Past the cap the preview stops, and says that it did - silently showing
    /// part of a change as if it were the whole one is the mistake the
    /// preview exists to prevent.
    #[test]
    fn a_change_past_the_cap_says_how_much_is_missing() {
        let new: String = (0..MAX_PREVIEW_LINES * 2)
            .map(|index| format!("line {index}\n"))
            .collect();

        let change = preview(Some(""), &new);

        assert!(change.body.contains("more line(s)"), "{}", change.body);
        assert!(change.body.lines().count() <= MAX_PREVIEW_LINES + 1);
    }

    /// A minified bundle is one line; it must not wrap the terminal forever.
    #[test]
    fn a_very_long_line_is_clipped() {
        let change = preview(None, &"x".repeat(MAX_PREVIEW_COLUMNS * 3));

        // The first line is the "(new file)" note; the second is the content.
        let body_line = change.body.lines().nth(1).unwrap();
        assert!(body_line.contains('…'), "{body_line}");
        assert!(body_line.chars().count() < MAX_PREVIEW_COLUMNS * 2);
    }

    /// The preview is the one place a file's contents are printed *because* a
    /// person asked to see them, so it goes through the same masking every
    /// other route to the screen does.
    #[test]
    fn a_secret_in_the_change_is_masked() {
        let change = preview(None, "AWS_SECRET_ACCESS_KEY=abcd1234efgh\n");

        assert!(!change.body.contains("abcd1234efgh"), "{}", change.body);
    }
}
