//! Command substitution has to come back as a value, not as terminal output.
//!
//! `$(...)` used to `fork()` from under the multi-threaded Tokio runtime, which
//! aborted the child inside Tokio's IO driver whenever stdin was a terminal, and
//! it handed the substitution pipe over as a bare `ctx.outfile`, which the
//! non-interactive auto-capture path overwrote: the caller read an empty string
//! while the inner command's output appeared on the terminal.

mod common;

use common::run_command;

fn stdout_of(command: &str) -> String {
    let output = run_command(command);
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn substitution_result_reaches_the_surrounding_command() {
    let stdout = stdout_of("echo $(/bin/echo hi)");
    assert!(
        stdout.lines().any(|line| line.trim() == "hi"),
        "expected the substitution result in {stdout:?}"
    );
}

#[test]
fn quoted_substitution_result_reaches_the_surrounding_command() {
    let stdout = stdout_of("echo \"$(/bin/echo hi)\"");
    assert!(
        stdout.lines().any(|line| line.trim() == "hi"),
        "expected the quoted substitution result in {stdout:?}"
    );
}

/// The inner command must not write to the shell's own stdout: seeing its output
/// once (as the result) rather than twice is what tells the two paths apart.
#[test]
fn inner_output_is_not_also_leaked_to_the_terminal() {
    let stdout = stdout_of("echo $(/bin/echo marker)");
    let occurrences = stdout
        .lines()
        .filter(|line| line.trim() == "marker")
        .count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one 'marker' in {stdout:?}"
    );
}

/// A stdin redirection inside the substitution used to overwrite the pipe the
/// caller was reading from, which hung the shell instead of returning.
#[test]
fn substitution_with_a_stdin_redirect_returns_instead_of_hanging() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().join("input.txt");
    std::fs::write(&path, "from-file\n").expect("failed to write input file");

    let stdout = stdout_of(&format!("echo $(cat < {})", path.display()));
    assert!(
        stdout.lines().any(|line| line.trim() == "from-file"),
        "expected the redirected substitution result in {stdout:?}"
    );
}

/// A result larger than a pipe buffer must not deadlock the job producing it.
///
/// The word count is piped through `wc` on purpose: the test harness collects
/// dsh's stdout only after the process exits, so printing the whole result here
/// would deadlock on the harness's own pipe rather than on anything in dsh.
#[test]
fn substitution_larger_than_the_pipe_buffer_completes() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().join("big.txt");
    let line_count = 20_000;
    let body: String = (0..line_count).map(|_| "0123456789\n").collect();
    std::fs::write(&path, &body).expect("failed to write big file");

    let stdout = stdout_of(&format!("/bin/echo $(cat {}) | wc -w", path.display()));
    assert!(
        stdout
            .lines()
            .any(|line| line.trim() == line_count.to_string()),
        "expected {line_count} words from the substitution result in {stdout:?}"
    );
}

/// Each job in a list starts from the caller's stdio: the substitution rework
/// resets it per job, and a second command must still reach the terminal.
#[test]
fn each_command_in_a_list_still_writes_to_the_terminal() {
    let stdout = stdout_of("/bin/echo one; /bin/echo two");
    for expected in ["one", "two"] {
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "expected a line {expected:?} in {stdout:?}"
        );
    }
}

/// A substitution is a list, not a single command: everything it prints belongs
/// in the result. Handing every job the same raw pipe let the first one close it
/// on its way out, so the second wrote to a closed descriptor and vanished.
#[test]
fn every_command_in_a_substitution_contributes_its_output() {
    let stdout = stdout_of("/usr/bin/printf '[%s]\\n' $(/bin/echo aaa; /bin/echo bbb)");
    for expected in ["[aaa]", "[bbb]"] {
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "expected {expected:?} in {stdout:?}"
        );
    }
}

/// `&&` and `||` gate the jobs inside a substitution the same way they gate a
/// top-level line.
#[test]
fn a_substitution_honours_and_or_gating() {
    let skipped = stdout_of("/bin/echo [$(/bin/false && /bin/echo X)]");
    assert!(
        !skipped.contains('X'),
        "`&&` ran the second command anyway: {skipped:?}"
    );

    let run = stdout_of("/bin/echo [$(/bin/true && /bin/echo X)]");
    assert!(
        run.contains('X'),
        "`&&` skipped the second command: {run:?}"
    );

    let short_circuited = stdout_of("/bin/echo [$(/bin/true || /bin/echo X)]");
    assert!(
        !short_circuited.contains('X'),
        "`||` ran the second command anyway: {short_circuited:?}"
    );
}

/// Running the substitution in-process means a builtin inside it writes to the
/// shell's own state. `cd` must not move the session that asked for the value.
#[test]
fn a_directory_change_inside_a_substitution_stays_inside_it() {
    let output = common::run_interactive(&["cd /tmp", "echo [$(cd /)]", "pwd"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "/tmp"),
        "the substitution moved the shell: {stdout:?}"
    );
}

/// Same for shell variables a builtin sets while the substitution runs.
#[test]
fn a_variable_exported_inside_a_substitution_stays_inside_it() {
    let output = common::run_interactive(&["echo [$(export ZQ_LEAK=leaked)]", "echo [$ZQ_LEAK]"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("leaked"),
        "the substitution leaked a variable: {stdout:?}"
    );
}

/// The body of a substitution is a command line, so it gets the same expansion
/// as any other. It used to be handed to the parser verbatim, which meant
/// nothing inside it was expanded at all.
#[test]
fn the_body_of_a_substitution_is_expanded() {
    let home = std::env::var("HOME").expect("HOME");

    for command in [
        "/bin/echo $(/bin/echo $HOME)",
        "/bin/echo $(/bin/echo ~)",
        "/bin/echo \"$(/bin/echo $HOME)\"",
    ] {
        let stdout = stdout_of(command);
        assert!(
            stdout.lines().any(|line| line.trim() == home),
            "{command:?} did not expand its body: {stdout:?}"
        );
    }
}

/// Expanding the body must not cost it its operators.
#[test]
fn an_expanded_substitution_body_keeps_its_pipeline() {
    let stdout = stdout_of("/bin/echo $(/bin/echo $HOME | /usr/bin/tr a A)");
    assert!(
        stdout.contains('A'),
        "the pipeline inside the substitution did not run: {stdout:?}"
    );
    assert!(
        !stdout.contains("tr"),
        "the pipe was lost and `tr` became an argument: {stdout:?}"
    );
}
