//! Noise gating and formatting for the proactive failure hint.
//!
//! After a command fails, the REPL offers a one-line hint (quick fix ghost
//! text or a "how to diagnose" note). Not every non-zero exit deserves one:
//! interrupts and "no match" exits are normal shell traffic, and repeating the
//! same hint for the same failure is nagging. The rules live here as pure
//! functions so they stay testable without a terminal.

use crate::completion::shell_token::{self, SeparatorMode};

/// Commands whose exit code 1 means "no result", not a failure worth hinting
/// about. `diff`/`cmp` report differences that way; the matchers report a
/// clean miss.
const EXIT_ONE_IS_NORMAL: &[&str] = &[
    "grep", "rg", "egrep", "fgrep", "test", "[", "diff", "cmp", "pgrep", "which",
];

/// Exit codes in 128+n signal territory. 130 (SIGINT) is the user pressing
/// Ctrl-C; the rest are kills and crashes a replacement command cannot fix.
const SIGNAL_EXIT_RANGE: std::ops::RangeInclusive<i32> = 129..=165;

/// Whether a failed command should produce a proactive hint.
///
/// `last_hinted` is the `(command, exit_code)` pair the previous hint was
/// shown for; the same failure twice in a row stays quiet after the first.
pub(crate) fn should_offer_hint(
    command: &str,
    exit_code: i32,
    last_hinted: Option<&(String, i32)>,
) -> bool {
    if exit_code == 0 || command.trim().is_empty() {
        return false;
    }
    if SIGNAL_EXIT_RANGE.contains(&exit_code) {
        return false;
    }
    if exit_code == 1
        && failing_command_token(command).is_some_and(|c| EXIT_ONE_IS_NORMAL.contains(&c.as_str()))
    {
        return false;
    }
    if last_hinted.is_some_and(|(cmd, code)| cmd == command && *code == exit_code) {
        return false;
    }
    true
}

/// Wrappers that run another command; the exit code comes from what follows,
/// so the gate has to look past them.
const COMMAND_WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "command", "time", "nice", "ionice", "nohup", "builtin", "exec",
];

/// The command the exit code actually came from.
///
/// A pipeline reports its *last* segment's status, so `cat f | grep x`
/// exiting 1 is grep's clean miss, not a `cat` failure — reading the first
/// token would gate exactly backwards. Leading `VAR=value` assignments and
/// wrappers such as `sudo` are skipped so they do not hide the real command.
fn failing_command_token(input: &str) -> Option<String> {
    let tokens: Vec<String> = shell_token::tokenize(input, SeparatorMode::Parser)
        .into_iter()
        .map(|token| token.raw)
        .collect();

    // An unquoted `|` survives tokenization as its own token (quoted ones keep
    // their quotes and never compare equal), so this only splits real pipes.
    let last_segment = tokens
        .rsplit(|token| token == "|")
        .next()
        .unwrap_or(&tokens);

    last_segment
        .iter()
        .find(|token| {
            !COMMAND_WRAPPERS.contains(&token.as_str())
                // `FOO=bar cmd` — an assignment, not the command.
                && !token
                    .split_once('=')
                    .is_some_and(|(name, _)| !name.is_empty() && !name.starts_with('-'))
        })
        .cloned()
}

/// Longest hint title before it gets elided. Keeps the annotation on one line
/// even before the width guard trims it entirely.
const MAX_TITLE_WIDTH: usize = 60;

/// One-line annotation drawn right-aligned next to the ghost text.
pub(crate) fn format_hint_annotation(title: Option<&str>, has_replacement: bool) -> String {
    match (title, has_replacement) {
        (Some(title), true) => format!(" 💡 {} · ⇥/Alt-f", flatten_title(title)),
        (None, true) => " 💡 suggested fix · ⇥/Alt-f".to_string(),
        // No replacement to accept: point at the manual AI actions instead.
        (_, false) => " 💡 Alt-f fix · Alt-d diagnose".to_string(),
    }
}

fn flatten_title(title: &str) -> String {
    let mut flat: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX_TITLE_WIDTH {
        flat = flat.chars().take(MAX_TITLE_WIDTH - 1).collect::<String>() + "…";
    }
    flat
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_signal_exits() {
        assert!(!should_offer_hint("sleep 100", 130, None));
        assert!(!should_offer_hint("long-task", 143, None));
        assert!(should_offer_hint("cargo build", 101, None));
    }

    #[test]
    fn suppresses_no_match_exit_one_for_matchers() {
        assert!(!should_offer_hint("grep foo bar.txt", 1, None));
        assert!(!should_offer_hint("rg pattern", 1, None));
        assert!(!should_offer_hint("diff a b", 1, None));
        // Exit 2 from grep is a real error (bad pattern, missing file).
        assert!(should_offer_hint("grep foo bar.txt", 2, None));
        // Other commands failing with 1 still get a hint.
        assert!(should_offer_hint("cargo test", 1, None));
    }

    #[test]
    fn matcher_suppression_follows_the_pipeline_exit_status() {
        // A pipeline exits with its LAST segment's status, so this exit 1 is
        // grep's clean miss and deserves no hint.
        assert!(!should_offer_hint("cat file | grep foo", 1, None));
        assert!(!should_offer_hint("ps aux | rg dsh", 1, None));
        assert!(!should_offer_hint("  grep foo", 1, None));
        // Conversely, a real failure after a matcher must not be swallowed.
        assert!(should_offer_hint("grep foo file | cargo build", 1, None));
        // A quoted `|` is data, not a pipe.
        assert!(should_offer_hint(r#"echo "a | grep""#, 1, None));
        assert!(should_offer_hint(r#""grep" foo"#, 1, None));
    }

    #[test]
    fn wrappers_and_assignments_do_not_hide_the_command() {
        assert!(!should_offer_hint("sudo grep foo /etc/shadow", 1, None));
        assert!(!should_offer_hint("env LC_ALL=C grep foo file", 1, None));
        assert!(!should_offer_hint("RUST_LOG=info grep foo", 1, None));
        assert!(!should_offer_hint("time diff a b", 1, None));
        // The wrapper itself is not a matcher, so a real failure still hints.
        assert!(should_offer_hint("sudo systemctl restart nope", 1, None));
    }

    #[test]
    fn same_failure_twice_hints_only_once() {
        let hinted = ("make build".to_string(), 2);
        assert!(!should_offer_hint("make build", 2, Some(&hinted)));
        assert!(should_offer_hint("make build", 1, Some(&hinted)));
        assert!(should_offer_hint("make test", 2, Some(&hinted)));
    }

    #[test]
    fn empty_or_successful_commands_never_hint() {
        assert!(!should_offer_hint("", 1, None));
        assert!(!should_offer_hint("   ", 1, None));
        assert!(!should_offer_hint("ls", 0, None));
    }

    #[test]
    fn annotation_formats() {
        assert_eq!(
            format_hint_annotation(Some("Git subcommand `chekout` → `checkout`"), true),
            " 💡 Git subcommand `chekout` → `checkout` · ⇥/Alt-f"
        );
        assert_eq!(
            format_hint_annotation(None, true),
            " 💡 suggested fix · ⇥/Alt-f"
        );
        assert_eq!(
            format_hint_annotation(None, false),
            " 💡 Alt-f fix · Alt-d diagnose"
        );
    }

    #[test]
    fn annotation_flattens_and_elides_long_titles() {
        let long = format!("line one\nline two {}", "x".repeat(80));
        let annotation = format_hint_annotation(Some(&long), true);
        assert!(!annotation.contains('\n'));
        assert!(annotation.contains('…'));
    }
}
