//! Phase 3: `$(...)`, `( ... )`, and `<(...)` bodies execute in re-exec
//! helpers on the shared protocol — true process isolation with per-job
//! runtime expansion, final `SafetyGuard` authorization, and explicit
//! fd/producer ownership in the parent.

mod common;

use std::fs;

fn stdout_of(command: &str) -> String {
    let output = common::run_command(command);
    assert!(output.status.success(), "command failed: {:?}", output);
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Full output with status: failure messages name stdout, stderr, and the
/// exit status so an empty producer pipe can be told apart from a consumer
/// that never ran.
fn run_full(command: &str) -> (String, String, std::process::ExitStatus) {
    let output = common::run_command(command);
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status,
    )
}

#[test]
fn command_substitution_does_not_move_parent_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_string_lossy().to_string();
    // `cd /` inside the substitution must resolve there, while the parent
    // stays where it was (canonicalized as needed).
    let out = stdout_of(&format!("cd {target}; echo $(cd /; pwd); pwd"));
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines.len(), 2, "unexpected lines: {lines:?}");
    assert_eq!(lines[0], "/", "substitution did not run at /: {lines:?}");
    assert!(
        lines[1].contains(dir.path().file_name().unwrap().to_str().unwrap()),
        "parent cwd moved by substitution: {lines:?}"
    );
}

#[test]
fn alias_mutation_inside_substitution_stays_inside() {
    let output = common::run_interactive(&[
        "alias dshisoax=outer",
        "echo $(alias dshisoax=inner; alias dshisoax)",
        "alias dshisoax",
    ]);
    assert!(output.status.success(), "command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("inner"),
        "helper did not see its own alias: {stdout:?}"
    );
    // The parent's final listing must still describe `outer`, never `inner`.
    let last_listing = stdout
        .lines()
        .rev()
        .find(|line| line.contains("dshisoax"))
        .unwrap_or("");
    assert!(
        last_listing.contains("outer"),
        "parent alias leaked from substitution: {stdout:?}"
    );
}

#[test]
fn nested_gating_inside_process_substitution() {
    let out = stdout_of("cat <(false && echo bad; echo good)");
    assert!(
        out.lines().any(|line| line.trim() == "good"),
        "gated body lost its good branch: {out:?}"
    );
    assert!(
        !out.lines().any(|line| line.trim() == "bad"),
        "skipped && branch ran: {out:?}"
    );
}

#[test]
fn skipped_process_substitution_spawns_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("skipped_marker");
    let output = common::run_command(&format!("false && cat <(touch {})", marker.display()));
    assert!(!marker.exists(), "skipped substitution ran: {:?}", output);
}

#[test]
fn repeated_process_substitution_does_not_hang_or_leak() {
    // Unique markers per iteration: a stale producer's output cannot be
    // mistaken for the current one, and one empty iteration fails loudly.
    for i in 0..20 {
        let word = format!("PS-{i:03}");
        let (out, err, status) = run_full(&format!("cat <(printf {word})"));
        assert!(
            status.success(),
            "iteration {word} failed: stdout={out:?} stderr={err:?} status={status:?}"
        );
        assert!(
            out.lines().any(|line| line.trim() == word),
            "iteration {word} broke: stdout={out:?} stderr={err:?} status={status:?}"
        );
    }
}

#[test]
fn two_producers_in_one_command() {
    let (out, err, status) = run_full("cat <(printf one) <(printf two)");
    assert!(
        status.success(),
        "two-producer command failed: stdout={out:?} stderr={err:?} status={status:?}"
    );
    assert!(
        out.contains("one"),
        "first producer missing: stdout={out:?} stderr={err:?} status={status:?}"
    );
    assert!(
        out.contains("two"),
        "second producer missing: stdout={out:?} stderr={err:?} status={status:?}"
    );
}

#[test]
fn three_producers_in_one_command() {
    // `cat` concatenates in argv order, so the expectation is fixed.
    let (out, err, status) = run_full("cat <(printf one) <(printf two) <(printf three)");
    assert!(
        status.success(),
        "three-producer command failed: stdout={out:?} stderr={err:?} status={status:?}"
    );
    for word in ["one", "two", "three"] {
        assert!(
            out.contains(word),
            "producer {word} missing: stdout={out:?} stderr={err:?} status={status:?}"
        );
    }
}

#[test]
fn empty_producer_is_eof_not_hang() {
    let (out, err, status) = run_full(&format!("cat <({})", common::true_path()));
    assert!(
        status.success(),
        "empty producer failed: stdout={out:?} stderr={err:?} status={status:?}"
    );
    assert!(
        out.trim().is_empty(),
        "empty producer must read as EOF: stdout={out:?} stderr={err:?} status={status:?}"
    );
}

#[test]
fn nonzero_producer_still_delivers_its_stream() {
    // A non-zero producer status must not discard the bytes already written.
    // `false` exits 1 after `printf` wrote, using only policy-allowed words
    // (`sh -c` would deny the whole producer by policy, which is separate
    // existing semantics, not data loss).
    let (out, err, status) = run_full("cat <(printf x; false)");
    assert!(
        out.contains('x'),
        "non-zero producer stream lost: stdout={out:?} stderr={err:?} status={status:?}"
    );
}

#[test]
fn slow_producer_is_not_read_as_early_eof() {
    // The consumer must block for a late producer, not settle on the empty
    // pipe it sees first. This is a regression test, not a timing fix.
    let (out, err, status) = run_full("cat <(sleep 0.05; printf late)");
    assert!(
        status.success(),
        "slow producer command failed: stdout={out:?} stderr={err:?} status={status:?}"
    );
    assert!(
        out.contains("late"),
        "slow producer read as early EOF: stdout={out:?} stderr={err:?} status={status:?}"
    );
}

#[test]
fn large_producer_streams_past_the_pipe_buffer() {
    // ~128 KiB exceeds the 64 KiB pipe buffer: the producer can only finish
    // while the consumer drains concurrently. Buffering the whole stream in
    // the parent first would deadlock here.
    //
    // The stream goes to a file, not the harness stdout pipe: the harness
    // only drains after exit, so >64 KiB on stdout would wedge the display
    // path regardless of producer/consumer streaming (separate pre-existing
    // limitation, out of scope here).
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("large.out");
    let (out, err, status) = run_full(&format!(
        "cat <({} | {} -c 131072) > {}",
        common::yes_path(),
        common::head_path(),
        target.display()
    ));
    assert!(
        status.success(),
        "large producer failed: stdout={out:?} stderr={err:?} status={status:?}"
    );
    let bytes = std::fs::read(&target).expect("read large output");
    assert_eq!(
        bytes.len(),
        131072,
        "large producer short: len={} stdout={out:?} stderr={err:?} status={status:?}",
        bytes.len(),
    );
}

#[test]
fn nested_glob_expands_at_helper_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("nested_a"), "a").expect("write");
    fs::write(dir.path().join("nested_b"), "b").expect("write");
    let out = stdout_of(&format!(
        "cd {}; echo $(echo nested_*)",
        dir.path().display()
    ));
    assert!(
        out.contains("nested_a"),
        "helper cwd glob missed a: {out:?}"
    );
    assert!(
        out.contains("nested_b"),
        "helper cwd glob missed b: {out:?}"
    );
}

#[test]
fn nested_dollar_question_mark_matches_top_level() {
    let out = stdout_of("echo $(false; echo $?)");
    assert!(
        out.lines().any(|line| line.trim() == "1"),
        "helper $? diverged: {out:?}"
    );
}

#[test]
fn sigpipe_producer_death_is_eof_not_hang() {
    // `yes` dies by SIGPIPE once `head` leaves; the consumer sees EOF and
    // the line completes. The producer's signal status flows through the one
    // shared `ProcessState` mapping in the reaper — no duplicated logic, no
    // hang, no zombie.
    let out = stdout_of(&format!("head -c 10 <({} )", common::yes_path()));
    // `head -c 10` over `y\n` streams: five `y` lines, then the producer
    // takes SIGPIPE once the consumer leaves. `trim` absorbs the shell's
    // pre-existing leading `\r\n` display prefix on `-c` output.
    assert_eq!(
        out.trim(),
        "y\ny\ny\ny\ny",
        "SIGPIPE cutover broke: {out:?}"
    );
}

#[test]
fn nested_pipeline_signal_resilience_in_capture() {
    let out = stdout_of(&format!(
        "echo \"$({} | {} -1)\"",
        common::yes_path(),
        common::head_path()
    ));
    assert!(
        out.lines().any(|line| line.trim() == "y"),
        "nested pipeline broke: {out:?}"
    );
}

#[test]
fn denied_kill_word_aborts_line_fail_closed() {
    // `kill` needs confirmation; with no terminal the helper denies and the
    // denial aborts the whole line (130) instead of running the outer
    // command on empty output. The denied command never executes.
    let output = common::run_command("echo \"$(sh -c 'kill -TERM $$')\"");
    assert_eq!(
        output.status.code(),
        Some(130),
        "denial did not abort the line: {:?}",
        output
    );
    assert!(
        output.stdout.is_empty(),
        "outer command ran on denied substitution: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn nested_process_substitution_inside_capture() {
    // `<(...)` inside `$(...)` materializes in the helper: its fds and
    // producer must attach to the helper job (not drop mid-iteration),
    // or the producer is never synchronously reaped.
    let out = stdout_of("echo $(cat <(printf nested-hi))");
    assert!(
        out.lines().any(|line| line.trim() == "nested-hi"),
        "nested producer output missing: {out:?}"
    );
}

#[test]
fn dropped_owned_fd_closes_for_real() {
    use std::os::fd::{FromRawFd, OwnedFd};

    let mut pipe_fds = [0 as std::os::unix::io::RawFd; 2];
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];
    {
        let _owned = unsafe { OwnedFd::from_raw_fd(read_fd) };
        // `OwnedFd` closes on drop: the fd must be gone afterwards on both
        // Linux and macOS, so the parent never leaks one fd per `<(...)`.
        assert_eq!(unsafe { libc::fcntl(read_fd, libc::F_GETFD) }, 0);
    }
    assert_eq!(
        unsafe { libc::fcntl(read_fd, libc::F_GETFD) },
        -1,
        "dropped OwnedFd left the descriptor open"
    );
    unsafe { libc::close(write_fd) };
}
