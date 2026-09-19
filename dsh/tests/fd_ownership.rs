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

/// The internal re-exec protocol reserves its descriptors dynamically, so the
/// `/dev/fd/N` handle handed to a `<(...)` consumer must survive the
/// producer-helper spawn no matter which number it holds.
#[test]
fn process_substitution_handle_survives_helper_spawn() {
    let output = run_command("cat <(printf FD-COLLISION-MARKER)");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stdout.contains("FD-COLLISION-MARKER"),
        "producer output lost across the re-exec boundary: stdout={stdout:?} stderr={stderr:?} status={:?}",
        output.status,
    );
}

/// Test B (consumer-only): no producer helper anywhere. A preseeded
/// non-CLOEXEC pipe is inherited through the normal external fork/exec path
/// as `/dev/fd/N`.
///
/// Green here + red end-to-end isolates the failure to the producer side or
/// to resource-lifetime ordering. Red here isolates it to external fd
/// inheritance / consumer lifetime, independent of the re-exec producer.
#[test]
fn consumer_inherits_preseeded_pipe_without_producer() {
    use std::io::Write as _;
    use std::os::fd::FromRawFd as _;

    // The `<(...)` shape: non-CLOEXEC read end, so `fork`+`execve` carries it
    // into the consumer. `run_command` spawns dogesh via `Command`, which
    // inherits every non-CLOEXEC fd open here; dogesh then forks its consumer
    // the same way.
    let mut pipe_fds = [0 as std::os::unix::io::RawFd; 2];
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];
    {
        let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd) };
        writer.write_all(b"CONSUMER-MARKER").expect("write marker");
        // Closed here: the consumer must see EOF right after the marker
        // instead of hanging on a held write end.
    }
    let output = run_command(&format!("/bin/cat /dev/fd/{read_fd}"));
    unsafe { libc::close(read_fd) };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "consumer failed: stdout={stdout:?} stderr={stderr:?} status={:?}",
        output.status,
    );
    assert!(
        stdout.contains("CONSUMER-MARKER"),
        "consumer lost preseeded pipe /dev/fd/{read_fd}: stdout={stdout:?} stderr={stderr:?} status={:?}",
        output.status,
    );
}

/// `$(...)` around `<(...)`: command substitution, process substitution, and
/// the nested helper/external below must all share the descriptor namespace
/// without collision.
#[test]
fn nested_command_and_process_substitution_share_no_descriptor() {
    let stdout = stdout_of("echo $(cat <(printf DEEP-MARKER))");
    assert!(
        stdout.lines().any(|line| line.trim() == "DEEP-MARKER"),
        "nested substitution output lost: {stdout:?}"
    );
}
