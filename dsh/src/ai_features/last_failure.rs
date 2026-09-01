//! One way to answer "what just failed?".
//!
//! Three call sites used to resolve this independently and two were wrong: the
//! Alt-d handler read only stderr, so a build tool that reports on stdout was
//! diagnosed from an empty string, and the command palette read only stdout and
//! passed a hard-coded exit code of 1. Both now come through here, which reads
//! the session's command blocks - the same record the `shell_history` chat tool
//! exposes - and falls back to `$OUT` only when no block matches.

use crate::environment::Environment;

/// The failed command a diagnosis is about.
pub struct LastFailure {
    pub command: String,
    pub exit_code: i32,
    /// stdout and stderr, labelled and concatenated.
    pub output: String,
}

/// Combine the two streams so the model can tell which one carried the error.
///
/// Labels are omitted when only one stream produced anything, because a single
/// unlabelled block is what every prompt here already expects.
pub fn combine_streams(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("stdout:\n{stdout}\n\nstderr:\n{stderr}"),
    }
}

/// Resolve the failure to diagnose.
///
/// `hint` is the `(command, exit_code)` the caller already knows - the REPL
/// tracks both. Without it the newest recorded block is used, which is what the
/// palette needs since it has no REPL state to read.
pub fn resolve(env: &Environment, hint: Option<(&str, i32)>) -> Option<LastFailure> {
    let environment = env;
    let blocks = &environment.session_output_state.command_blocks;

    // `iter()` is newest-first.
    let matched = match hint {
        Some((command, exit_code)) => blocks
            .iter()
            .find(|block| block.command == command && block.exit_code == exit_code)
            .or_else(|| blocks.iter().find(|block| block.command == command)),
        // Without a hint the caller means "the thing that went wrong", so a
        // failure outranks whatever ran after it. Taking the newest block
        // outright meant running `ls` after a failed build made the palette
        // explain the successful `ls`.
        None => blocks
            .iter()
            .find(|block| block.exit_code != 0)
            .or_else(|| blocks.iter().next()),
    };

    if let Some(block) = matched {
        return Some(LastFailure {
            command: block.command.clone(),
            exit_code: block.exit_code,
            output: combine_streams(&block.stdout, &block.stderr),
        });
    }

    // No block: the command ran before block recording, or was filtered out.
    // `$OUT` still holds the last captured output.
    let (command, exit_code) = hint?;
    Some(LastFailure {
        command: command.to_string(),
        exit_code,
        output: environment.get_var("OUT").unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_labels_only_when_both_streams_spoke() {
        assert_eq!(combine_streams("out", ""), "out");
        assert_eq!(combine_streams("", "err"), "err");
        assert_eq!(combine_streams("  ", "  "), "");
        assert_eq!(
            combine_streams("out", "err"),
            "stdout:\nout\n\nstderr:\nerr"
        );
    }
}
