//! Deciding whether a foreground command is a Herdr-recognized agent CLI.
//!
//! Pure functions only - no environment or process state is read directly
//! here. Callers resolve settings through [`crate::environment::Environment`]
//! (shell variable, then process environment - see
//! `docs/ai/skills/doge-shell-repo/references/env-vars.md`), matching every
//! other AI-feature setting in this shell instead of a bespoke
//! `std::env::var` lookup.

/// Commands Herdr recognizes as coding agents, keyed by CLI binary name. The
/// canonical list is `herdr agent`'s own `kinds:` line; keep this in sync
/// with it rather than guessing.
///
/// A few of these (`pi`, `amp`, `omp`, `agy`, `muse`, `maki`, `cursor`)
/// collide with unrelated tool names on some systems. That's an acceptable
/// false positive: the only cost is that pane authority is on loan to
/// Herdr's own (empty) detection for the duration of that one command, which
/// self-heals the moment the command exits. `DSH_HERDR_AGENT_COMMANDS` lets a
/// user drop one of these (`-pi`) or replace the whole list.
const BUILTIN_AGENT_COMMANDS: &[&str] = &[
    "agy",
    "amp",
    "claude",
    "cline",
    "codex",
    "copilot",
    "cursor",
    "cursor-agent",
    "devin",
    "droid",
    "gemini",
    "grok",
    "hermes",
    "kilo",
    "kimi",
    "kiro",
    "maki",
    "mastracode",
    "muse",
    "omp",
    "opencode",
    "pi",
    "qodercli",
    "qwen",
];

/// Extends (or, with a leading `-name` entry, excludes from) the builtin
/// agent command list. `:`-separated, like `PATH`/`Z_EXCLUDE`.
pub(crate) const AGENT_COMMANDS_KEY: &str = "DSH_HERDR_AGENT_COMMANDS";

/// Set to `0`/`false`/`off`/`no` to disable the whole handoff feature, even
/// when Herdr is active.
pub(crate) const HANDOFF_KEY: &str = "DSH_HERDR_AGENT_HANDOFF";

/// Whether `name` (already a basename, e.g. `codex` not `/usr/bin/codex`)
/// should trigger a pane authority handoff. `configured` is the raw
/// `DSH_HERDR_AGENT_COMMANDS` value, if any.
pub(crate) fn is_agent_command(name: &str, configured: Option<&str>) -> bool {
    let (additions, exclusions) = split_configured(configured.unwrap_or(""));
    if exclusions.iter().any(|excluded| eq(excluded, name)) {
        return false;
    }
    BUILTIN_AGENT_COMMANDS.iter().any(|known| eq(known, name))
        || additions.iter().any(|added| eq(added, name))
}

/// Whether the handoff feature itself is enabled. `configured` is the raw
/// `DSH_HERDR_AGENT_HANDOFF` value, if any.
pub(crate) fn handoff_enabled(configured: Option<&str>) -> bool {
    match configured {
        None => true,
        Some(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Splits a `:`-separated `DSH_HERDR_AGENT_COMMANDS` value into additions and
/// exclusions (a `-`-prefixed entry). Blank entries (repeated `:`, leading or
/// trailing whitespace, or a bare `-` with nothing after it) are dropped
/// rather than matching everything or nothing by accident.
fn split_configured(raw: &str) -> (Vec<&str>, Vec<&str>) {
    let mut additions = Vec::new();
    let mut exclusions = Vec::new();
    for entry in raw.split(':') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.strip_prefix('-') {
            Some("") => continue,
            Some(excluded) => exclusions.push(excluded),
            None => additions.push(entry),
        }
    }
    (additions, exclusions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_match_case_insensitively() {
        assert!(is_agent_command("codex", None));
        assert!(is_agent_command("Codex", None));
        assert!(is_agent_command("CLAUDE", None));
    }

    #[test]
    fn unrelated_names_do_not_match() {
        assert!(!is_agent_command("code", None));
        assert!(!is_agent_command("echo", None));
        assert!(!is_agent_command("git", None));
    }

    #[test]
    fn configured_value_adds_extra_names() {
        assert!(is_agent_command("mycli", Some("mycli")));
        assert!(is_agent_command("codex", Some("mycli")));
        assert!(is_agent_command("othertool", Some("mycli:othertool")));
    }

    #[test]
    fn a_dash_prefixed_entry_excludes_a_builtin_name() {
        assert!(!is_agent_command("pi", Some("-pi")));
        assert!(is_agent_command("codex", Some("-pi")));
    }

    #[test]
    fn an_empty_configured_value_changes_nothing() {
        assert!(is_agent_command("codex", Some("")));
        assert!(!is_agent_command("code", Some("")));
    }

    #[test]
    fn blank_entries_between_colons_are_ignored() {
        assert!(is_agent_command("codex", Some("::mycli::")));
        assert!(is_agent_command("mycli", Some("::mycli::")));
    }

    #[test]
    fn a_bare_dash_entry_is_dropped_rather_than_added_as_a_literal_name() {
        // Regression test: `strip_prefix('-')` on a bare "-" yields
        // `Some("")`, which must not fall through to being pushed as an
        // addition literally named "-".
        assert!(!is_agent_command("-", Some("-")));
        assert!(is_agent_command("codex", Some("-")));
        assert!(is_agent_command("codex", Some("mycli:-")));
    }

    #[test]
    fn handoff_enabled_defaults_to_true() {
        assert!(handoff_enabled(None));
        assert!(handoff_enabled(Some("1")));
        assert!(handoff_enabled(Some("yes")));
    }

    #[test]
    fn handoff_can_be_disabled() {
        for value in ["0", "false", "off", "no", "OFF", " 0 "] {
            assert!(
                !handoff_enabled(Some(value)),
                "expected {value:?} to disable"
            );
        }
    }
}
