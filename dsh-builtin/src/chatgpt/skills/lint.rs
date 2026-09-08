//! Whether what `skill_manage` is about to write will actually reach the agent.
//!
//! `validate()` in `tool/skill.rs` checks the request (name, scope, path). It
//! does not look at the content being written, so a `patch` that deletes the
//! `description:` line went through cleanly and the skill it touched fell out
//! of the prompt on the very next turn - `summary()` falls back to the first
//! body line, and a skill with no frontmatter `description` is reported as
//! broken by `doctor skills`, not refused up front.
//!
//! This runs in two places: here, on the content `skill_manage` is about to
//! write, before the user is asked to approve anything; and from `doctor`,
//! reading a skill already on disk. Both call the same checks so a skill that
//! passes when written also passes when audited later.
//!
//! Deliberately not run on the load path (`load_root_reporting`). That path
//! has to accept whatever a human wrote by hand, or a skill authored for a
//! different tool; being picky about frontmatter there would stop something
//! from listing at all instead of asking the writer to fix it.

use super::{MAX_DESCRIPTION_CHARS, MAX_SKILL_SUMMARY_CHARS, frontmatter_field, split_frontmatter};

/// A file's contents large enough that it belongs in `references/` instead.
const MAX_BODY_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LintLevel {
    /// Refuse the write outright, before the user is asked anything.
    Reject,
    /// Write it, but say why it may not work as intended.
    Warn,
}

#[derive(Debug, Clone)]
pub(crate) struct LintFinding {
    pub(crate) level: LintLevel,
    pub(crate) message: String,
}

impl LintFinding {
    fn reject(message: impl Into<String>) -> Self {
        Self {
            level: LintLevel::Reject,
            message: message.into(),
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            level: LintLevel::Warn,
            message: message.into(),
        }
    }
}

pub(crate) fn has_rejection(findings: &[LintFinding]) -> Option<&str> {
    findings
        .iter()
        .find(|finding| finding.level == LintLevel::Reject)
        .map(|finding| finding.message.as_str())
}

pub(crate) fn warnings(findings: Vec<LintFinding>) -> Vec<String> {
    findings
        .into_iter()
        .filter(|finding| finding.level == LintLevel::Warn)
        .map(|finding| finding.message)
        .collect()
}

/// Lint the final contents of a skill's `SKILL.md`, after any patch has
/// already been applied - this sees what would land on disk, not the diff.
pub(crate) fn lint_skill_md(name: &str, contents: &str) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let (frontmatter, body) = split_frontmatter(contents);

    let Some(frontmatter) = frontmatter else {
        findings.push(LintFinding::reject(
            "SKILL.md must start with a `---` frontmatter block holding `name` and `description`",
        ));
        return findings;
    };

    match frontmatter_field(Some(frontmatter), "name") {
        Some(declared) if declared == name => {}
        Some(declared) => findings.push(LintFinding::reject(format!(
            "frontmatter `name: {declared}` does not match the skill `{name}`; the loader reports this as broken instead of showing it"
        ))),
        None => findings.push(LintFinding::reject(if nested_key(frontmatter, "name") {
            "frontmatter `name:` is indented under another key, so the reader cannot see it; move it to the top level".to_string()
        } else {
            "frontmatter is missing `name:`".to_string()
        })),
    }

    match frontmatter_field(Some(frontmatter), "description") {
        Some(description) if !description.trim().is_empty() => {
            let chars = description.chars().count();
            if chars > MAX_DESCRIPTION_CHARS {
                findings.push(LintFinding::reject(format!(
                    "description is {chars} characters, over the {MAX_DESCRIPTION_CHARS}-character limit; shorten it and write again"
                )));
            } else if chars > MAX_SKILL_SUMMARY_CHARS {
                findings.push(LintFinding::warn(format!(
                    "description is {chars} characters; only the first {MAX_SKILL_SUMMARY_CHARS} reach the prompt, put the trigger first"
                )));
            }
        }
        _ => findings.push(LintFinding::reject(if nested_key(frontmatter, "description") {
            "frontmatter `description:` is indented under another key, so the reader cannot see it; move it to the top level".to_string()
        } else {
            "frontmatter is missing a non-empty `description:`; that line is the only thing that tells the model when to use this skill".to_string()
        })),
    }

    lint_body(body, &mut findings);
    findings
}

/// A lighter pass for a bundled file (`references/*.md`, `scripts/*`, ...).
///
/// These are never parsed for frontmatter - they are named in the prompt, not
/// summarized - so there is nothing to reject here, only a nudge.
pub(crate) fn lint_bundled(relative: &str, contents: &str) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    if contents.trim().is_empty() {
        findings.push(LintFinding::warn(format!("`{relative}` is empty")));
    } else if contents.len() > MAX_BODY_BYTES {
        findings.push(LintFinding::warn(format!(
            "`{relative}` is {} bytes; skills are read for a specific need, keep bundled files short",
            contents.len()
        )));
    }
    findings
}

fn lint_body(body: &str, findings: &mut Vec<LintFinding>) {
    let trimmed = body.trim();
    let meaningful_lines = trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();

    if trimmed.is_empty() {
        findings.push(LintFinding::warn(
            "the body is empty; record the procedure, not just the frontmatter",
        ));
    } else if meaningful_lines == 1 && trimmed.trim_start().starts_with('#') {
        findings.push(LintFinding::warn(
            "the body is just a heading; add the steps a later run would follow",
        ));
    }

    if body.len() > MAX_BODY_BYTES {
        findings.push(LintFinding::warn(format!(
            "the body is {} bytes; move the long parts into `references/` and keep SKILL.md short",
            body.len()
        )));
    }
}

/// Whether `key:` appears in the frontmatter only on an indented line.
///
/// `frontmatter_field` deliberately does not read those - a `description:`
/// nested under `metadata:` is not the skill's own summary - but the writer
/// gets a more useful answer than "missing" when that is what happened.
/// Uses `super::is_indented`, the same predicate `frontmatter_field` skips
/// on, so this can never call a line "nested" that the reader did not.
fn nested_key(frontmatter: &str, key: &str) -> bool {
    frontmatter.lines().any(|line| {
        super::is_indented(line)
            && line
                .trim_start()
                .split_once(':')
                .is_some_and(|(field, _)| field.trim() == key)
    })
}

/// The `doctor skills` deep pass: read what is actually on disk and lint it,
/// including whether the relative links inside it resolve.
///
/// Not called from `load_root_reporting` - that path runs on every turn and
/// has to stay cheap; this one reads files and is only ever invoked from
/// `doctor`.
pub(crate) fn lint_path(skill_dir: &std::path::Path, name: &str) -> Vec<LintFinding> {
    let skill_md_path = if skill_dir.is_dir() {
        skill_dir.join("SKILL.md")
    } else {
        skill_dir.to_path_buf()
    };

    let Ok(contents) = std::fs::read_to_string(&skill_md_path) else {
        return vec![LintFinding::reject(format!(
            "cannot read {}",
            skill_md_path.display()
        ))];
    };

    let mut findings = lint_skill_md(name, &contents);
    let base = skill_md_path.parent().unwrap_or(skill_dir);
    findings.extend(lint_relative_links(base, &contents));
    findings
}

/// Markdown links to a relative path (`references/x.md`, `scripts/y.sh`)
/// whose target does not exist. A hand-written skill pointing at a file that
/// was never added is silent otherwise: the model is only told the names of
/// what is actually in the directory, never what the body claims is there.
fn lint_relative_links(base: &std::path::Path, contents: &str) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let mut rest = contents;

    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        rest = &rest[end + 1..];

        // Only a bare relative path is ours to check: no scheme (`mailto:`,
        // `https:`, ...), no anchor fragment, no absolute path. A link inside
        // an inline code span - shown as syntax, not meant to resolve - is
        // not distinguished from a real one; that is a known gap in this
        // heuristic, accepted because it can only under-warn, never refuse.
        if target.is_empty()
            || target.starts_with(['#', '/', '~'])
            || target.starts_with('<')
            || has_uri_scheme(target)
        {
            continue;
        }

        let target_path = target.split('#').next().unwrap_or(target);
        if !base.join(target_path).exists() {
            findings.push(LintFinding::warn(format!(
                "links to `{target_path}`, which does not exist in this skill"
            )));
        }
    }

    findings
}

/// Whether `target` starts with `scheme:` (`mailto:`, `tel:`, `https:`, ...)
/// rather than being a relative path. The same rule CommonMark uses to
/// recognize an autolink's scheme, not a full URI parser - good enough to
/// keep a `mailto:` link from being checked against the filesystem.
fn has_uri_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    let scheme = &target[..colon];
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_md(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n")
    }

    #[test]
    fn a_well_formed_skill_has_no_findings() {
        let contents = skill_md(
            "rust-bisect",
            "Use when bisecting a Rust regression.",
            "# rust-bisect\n\nDo the thing.",
        );
        assert!(lint_skill_md("rust-bisect", &contents).is_empty());
    }

    #[test]
    fn missing_frontmatter_is_rejected() {
        let findings = lint_skill_md("rust-bisect", "# rust-bisect\n\nNo frontmatter here.\n");
        assert_eq!(
            has_rejection(&findings),
            Some(
                "SKILL.md must start with a `---` frontmatter block holding `name` and `description`"
            )
        );
    }

    #[test]
    fn a_mismatched_name_is_rejected() {
        let contents = skill_md("other-name", "Use when needed.", "body");
        let findings = lint_skill_md("rust-bisect", &contents);
        assert!(has_rejection(&findings).is_some());
    }

    #[test]
    fn a_missing_description_is_rejected() {
        let contents = "---\nname: rust-bisect\n---\n\nbody\n";
        let findings = lint_skill_md("rust-bisect", contents);
        assert_eq!(
            has_rejection(&findings),
            Some(
                "frontmatter is missing a non-empty `description:`; that line is the only thing that tells the model when to use this skill"
            )
        );
    }

    #[test]
    fn an_empty_description_is_rejected() {
        let contents = skill_md("rust-bisect", "", "body");
        let findings = lint_skill_md("rust-bisect", &contents);
        assert!(has_rejection(&findings).is_some());
    }

    #[test]
    fn a_nested_description_names_the_real_problem() {
        let contents = "---\nname: rust-bisect\nmetadata:\n  description: nested under metadata\n---\n\nbody\n";
        let findings = lint_skill_md("rust-bisect", contents);
        let message = has_rejection(&findings).expect("rejection");
        assert!(message.contains("indented under another key"), "{message}");
    }

    #[test]
    fn a_description_over_the_hard_limit_is_rejected() {
        let long = "a".repeat(MAX_DESCRIPTION_CHARS + 1);
        let contents = skill_md("rust-bisect", &long, "body");
        let findings = lint_skill_md("rust-bisect", &contents);
        assert!(has_rejection(&findings).is_some());
    }

    #[test]
    fn a_description_over_the_prompt_budget_only_warns() {
        let long = "a".repeat(MAX_SKILL_SUMMARY_CHARS + 5);
        let contents = skill_md("rust-bisect", &long, "body");
        let findings = lint_skill_md("rust-bisect", &contents);
        assert!(has_rejection(&findings).is_none());
        assert!(!warnings(findings).is_empty());
    }

    #[test]
    fn an_empty_body_only_warns() {
        let contents = skill_md("rust-bisect", "Use when needed.", "");
        let findings = lint_skill_md("rust-bisect", &contents);
        assert!(has_rejection(&findings).is_none());
        assert!(!warnings(findings).is_empty());
    }

    #[test]
    fn a_heading_only_body_only_warns() {
        let contents = skill_md("rust-bisect", "Use when needed.", "# rust-bisect");
        let findings = lint_skill_md("rust-bisect", &contents);
        assert!(has_rejection(&findings).is_none());
        assert!(!warnings(findings).is_empty());
    }

    #[test]
    fn a_bundled_reference_file_is_never_rejected() {
        let findings = lint_bundled("references/api.md", "");
        assert!(has_rejection(&findings).is_none());
        assert!(!warnings(findings).is_empty());
    }

    #[test]
    fn lint_path_reports_a_broken_relative_link() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("rust-bisect");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let contents = skill_md(
            "rust-bisect",
            "Use when needed.",
            "See [details](references/missing.md).",
        );
        std::fs::write(skill_dir.join("SKILL.md"), &contents).unwrap();

        let findings = lint_path(&skill_dir, "rust-bisect");
        assert!(
            warnings(findings)
                .iter()
                .any(|message| message.contains("references/missing.md"))
        );
    }

    #[test]
    fn lint_path_is_quiet_when_the_link_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("rust-bisect");
        std::fs::create_dir_all(skill_dir.join("references")).unwrap();
        let contents = skill_md(
            "rust-bisect",
            "Use when needed.",
            "See [details](references/api.md).",
        );
        std::fs::write(skill_dir.join("SKILL.md"), &contents).unwrap();
        std::fs::write(skill_dir.join("references/api.md"), "notes").unwrap();

        let findings = lint_path(&skill_dir, "rust-bisect");
        assert!(has_rejection(&findings).is_none());
        assert!(warnings(findings).is_empty());
    }

    #[test]
    fn a_mailto_link_is_not_checked_against_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("rust-bisect");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let contents = skill_md(
            "rust-bisect",
            "Use when needed.",
            "Contact [the owner](mailto:owner@example.com) with questions.",
        );
        std::fs::write(skill_dir.join("SKILL.md"), &contents).unwrap();

        let findings = lint_path(&skill_dir, "rust-bisect");
        assert!(warnings(findings).is_empty());
    }

    /// The canonical skills under `docs/ai/skills/` are this lint's corpus:
    /// whatever it rejects here, `skill_manage` would also have refused to
    /// write. A regression here is either a bad canonical skill or a lint
    /// that has become too strict for skills this repository ships.
    #[test]
    fn the_repositorys_own_skills_pass_the_lint() {
        let source_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("docs")
            .join("ai")
            .join("skills");
        let entries = std::fs::read_dir(&source_root)
            .unwrap_or_else(|err| panic!("{}: {err}", source_root.display()));

        let mut checked = 0usize;
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let findings = lint_path(&entry.path(), &name);
            assert!(
                has_rejection(&findings).is_none(),
                "{name}: {:?}",
                has_rejection(&findings)
            );
            checked += 1;
        }
        assert!(
            checked >= 10,
            "expected to check the repository's own skills, found {checked}"
        );
    }
}
