//! Descriptors the caller hands a job belong to the caller.
//!
//! `Job::launch` used to close whatever a process ended up with unless it was
//! literally fd 0/1/2, which meant the capture pipe behind `|>` was gone after
//! the first stage of a pipeline, and a `ctx` reused across lines kept naming a
//! descriptor the previous line had closed.

mod common;

use common::{run_command, run_interactive};

fn stdout_of(command: &str) -> String {
    String::from_utf8_lossy(&run_command(command).stdout).to_string()
}

/// `|>` captures the *pipeline's* output, so the capture pipe has to survive
/// every stage being launched.
#[test]
fn capture_operator_works_at_the_end_of_a_pipeline() {
    let stdout = stdout_of("/bin/echo hi | /usr/bin/wc -c |>");
    assert!(
        stdout.lines().any(|line| line.trim() == "3"),
        "expected the byte count from the captured pipeline in {stdout:?}"
    );
}

/// Script mode drives every line through one `Context`, so a job that rewires it
/// has to put it back. `two` used to fail with `failed to duplicate file
/// descriptor` because the previous line had left `ctx.outfile` closed.
#[test]
fn a_rewired_context_does_not_survive_into_the_next_line() {
    let output = run_interactive(&["/bin/echo one", "/bin/echo two 2>&1", "/bin/echo three"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    for expected in ["one", "two", "three"] {
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "expected a line {expected:?} in {stdout:?}"
        );
    }
}

/// A redirect that fails halfway used to leave `ctx` naming a descriptor its own
/// error path had just closed. The next `pipe()` got that number back and the
/// shell closed it twice, aborting the process with an IO-safety violation.
#[test]
fn a_failed_redirect_leaves_the_shell_running() {
    let output = run_interactive(&[
        "/bin/echo BEFORE",
        "/bin/ls / 2>&1 > /nonexistent-directory-for-dsh/nope",
        "/bin/echo AFTER",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_ne!(
        output.status.code(),
        Some(134),
        "the shell aborted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for expected in ["BEFORE", "AFTER"] {
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "expected a line {expected:?} in {stdout:?}"
        );
    }
}
