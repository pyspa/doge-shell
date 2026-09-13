//! Parsing a skill's `SKILL.md` frontmatter and summary: the flat `key: value` reader (`frontmatter_field`), the block that splits it from the body (`split_frontmatter`), and the one-line summary shown in the
//! system prompt before the full body is read.

/// Returns `(raw, truncated_for_prompt)`. The raw half is what the trust
/// digest hashes; the truncated half is what the prompt fragment shows.
/// Collapsed to one line; not yet truncated for the prompt - `Skill::summary`
/// does that on demand.
pub(super) fn extract_skill_summary(instruction: &str) -> String {
    let (frontmatter, body) = split_frontmatter(instruction);
    if let Some(description) = frontmatter_field(frontmatter, "description") {
        return collapse_whitespace(&description);
    }

    let body_summary = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("No description available.");
    collapse_whitespace(body_summary)
}
pub(super) fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let mut offset = 0usize;
    let mut lines = content.split_inclusive('\n');

    let Some(first) = lines.next() else {
        return (None, content);
    };
    offset += first.len();

    if first.trim() != "---" {
        return (None, content);
    }

    for line in lines {
        offset += line.len();
        if line.trim() == "---" {
            let frontmatter = &content[first.len()..offset - line.len()];
            let body = &content[offset..];
            return (Some(frontmatter), body);
        }
    }

    (None, content)
}
/// Whether `line` is indented and therefore not a top-level frontmatter key.
///
/// Shared between `frontmatter_field` (which skips such a line) and
/// `lint::nested_key` (which explains to a writer why it was skipped), so the
/// two can never disagree about what counts as nested.
pub(super) fn is_indented(line: &str) -> bool {
    line.starts_with([' ', '\t'])
}
/// Read one top-level scalar out of the frontmatter.
///
/// Deliberately not a YAML parser. The only writer that has to round-trip
/// through it is `skill_manage`, which emits a flat `name`/`description` pair,
/// and every skill shipped with the repository is flat too. What it does have to
/// get right is the two shapes that silently produced the wrong answer: an
/// indented key belonging to some other mapping, and a block scalar whose value
/// starts on the following line.
pub(super) fn frontmatter_field(frontmatter: Option<&str>, key: &str) -> Option<String> {
    let frontmatter = frontmatter?;
    let mut lines = frontmatter.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Only top-level keys. Without this a `description:` nested under
        // `metadata:` was read as if it were the skill's own summary.
        if is_indented(line) {
            continue;
        }

        let Some((field, value)) = trimmed.split_once(':') else {
            continue;
        };
        if field.trim() != key {
            continue;
        }

        let value = value.trim();
        if !value.is_empty() && !matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
            return Some(strip_matching_quotes(value).to_string());
        }

        // A block scalar, or a key whose value is on the following lines. The
        // result is only ever rendered as one collapsed line, so the difference
        // between folding and literal blocks does not matter here.
        let mut collected = String::new();
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                lines.next();
                continue;
            }
            if !next.starts_with([' ', '\t']) {
                break;
            }
            collected.push(' ');
            collected.push_str(next.trim());
            lines.next();
        }

        let collected = collected.trim().to_string();
        return (!collected.is_empty()).then_some(collected);
    }

    None
}
fn strip_matching_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }

    value
}
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
/// Cut `text` to `max_chars`, appending `...` if it did not already fit.
///
/// Shared with `skill list` (`crate::skill`), which truncates to a narrower,
/// terminal-column budget than the prompt's own - two different constraints
/// on the same description, not two different truncation rules.
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    format!("{}...", &text[..end])
}
