//! The grammar has no `EOI` anchor, so `Rule::commands` can return a partial
//! match and the shell would run only the prefix of what the user typed. These
//! tests pin the guardrail: leftover input is reported, and ordinary commands
//! never trip it.

mod common;

use common::run_command;

const WARNING: &str = "dsh: warning: ignored unparsed input:";

fn stderr_of(command: &str) -> String {
    String::from_utf8_lossy(&run_command(command).stderr).into_owned()
}

/// Inputs that stay malformed no matter how far the parser work goes, so these
/// assertions are stable across the rest of the foundation stages.
#[test]
fn malformed_input_is_reported_instead_of_silently_truncated() {
    for (command, tail) in [
        ("echo a )", ")"),
        ("echo a &&&", "&"),
        ("echo a (((", "((("),
        ("echo unterminated\"", "\""),
    ] {
        let stderr = stderr_of(command);
        assert!(
            stderr.contains(WARNING),
            "expected a warning for {command:?}, got stderr:\n{stderr}"
        );
        assert!(
            stderr.contains(tail),
            "expected the warning for {command:?} to name the leftover {tail:?}, got stderr:\n{stderr}"
        );
    }
}

#[test]
fn ordinary_commands_do_not_warn() {
    for command in [
        "echo hello",
        "echo a; echo b",
        "echo a && echo b",
        "echo a || echo b",
        "echo a | cat",
        "echo 'single quoted'",
        "echo \"double quoted\"",
        "echo x > /dev/null",
        "echo trailing;",
        "echo spaced   ",
    ] {
        let stderr = stderr_of(command);
        assert!(
            !stderr.contains(WARNING),
            "unexpected unparsed-input warning for {command:?}:\n{stderr}"
        );
    }
}

/// The warning must not swallow the part that *did* parse — the prefix still
/// runs, which is exactly why the user needs to be told about the rest.
#[test]
fn the_parsed_prefix_still_runs() {
    let output = run_command("echo kept )");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("kept"),
        "the parsed prefix should still execute"
    );
}
