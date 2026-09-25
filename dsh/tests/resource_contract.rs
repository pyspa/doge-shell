//! Layer 3: resource ownership / concurrency harness.
//!
//! This layer owns real child processes, pids, pgids, fds, and `Shell`
//! ownership. It never asserts shell *semantics* (Layer 1) and never models
//! lifecycle states (Layer 2). Every test here answers one question: after
//! the shell is gone, is every helper, producer, and descriptor gone with
//! the shell that owned it?
//!
//! Rules:
//!
//! - Ordinary tests stay on the serial runner (`run_command`); only the
//!   explicit concurrency test uses [`spawn_dsh_unlocked`].
//! - Every spawned group is drained with a bound, and leaks `SIGKILL` the
//!   group before failing so CI never inherits strays.
//! - No `sleep`-based readiness: markers travel through pipes/stdout and
//!   every wait is bounded.

mod common;

use std::time::Duration;

use common::process::{
    DEFAULT_CASE_TIMEOUT, spawn_dsh_unlocked, spawn_dsh_unlocked_with_nofile_limit,
};
use common::{head_path, run_command, serial_guard, tr_path, yes_path};

const DRAIN_TIMEOUT: Duration = DEFAULT_CASE_TIMEOUT;

/// Shell exit leaves no survivors in its group: simple command.
#[test]
fn group_drains_after_simple_command() {
    let _serial = serial_guard();
    let process = spawn_dsh_unlocked(["-c", "echo drained-simple"], None);
    let output = process
        .assert_group_drained(DRAIN_TIMEOUT)
        .expect("group must drain after a simple command");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("drained-simple"));
}

/// Shell exit leaves no survivors after pipeline + substitution traffic.
#[test]
fn group_drains_after_pipeline_and_substitution() {
    let _serial = serial_guard();
    let script = format!(
        "echo drained-pipe | {} a-z A-Z; cat <(printf drained-sub)",
        tr_path()
    );
    let process = spawn_dsh_unlocked(["-c", &script], None);
    let output = process
        .assert_group_drained(DRAIN_TIMEOUT)
        .expect("group must drain after pipeline and substitution");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.contains("DRAINED-PIPE"), "got {stdout:?}");
    assert!(stdout.contains("drained-sub"), "got {stdout:?}");
}

/// Early consumer exit (`yes | head`) completes promptly, kills no sibling
/// artificially, and leaves no descendant behind.
#[test]
fn early_consumer_exit_leaves_no_strays() {
    let _serial = serial_guard();
    let script = format!("{} | {} -n 1; echo EARLY:$?", yes_path(), head_path());
    let process = spawn_dsh_unlocked(["-c", &script], None);
    let output = process
        .assert_group_drained(Duration::from_secs(8))
        .expect("group must drain after yes | head -n 1");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.contains("EARLY:0"), "got {stdout:?}");
}

/// Background producer stays owned until the job completes: a slow producer
/// behind `<(...)` in a background job still delivers its bytes.
#[test]
fn background_producer_owned_until_job_completes() {
    let _serial = serial_guard();
    let script = "printf 'sleep 1\\nprintf DELAYED-MARKER\\n' > delayed.sh; cat <(sh delayed.sh) & echo BG:$?";
    let process = spawn_dsh_unlocked(["-c", script], None);
    let output = process
        .assert_group_drained(Duration::from_secs(10))
        .expect("group must drain after background producer job");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.contains("BG:0"), "got {stdout:?}");
    assert!(stdout.contains("DELAYED-MARKER"), "got {stdout:?}");
}

/// Producer/capture boundary sizes. Large outputs are captured and counted,
/// never dumped raw to the test terminal.
#[test]
fn output_boundary_sizes_are_exact() {
    for (script, expected) in [
        ("printf '' | wc -c", "0"),
        ("printf 'x' | wc -c", "1"),
        ("printf 'nonewline10' | wc -c", "11"),
        ("echo hello | wc -c", "6"),
        (
            &format!("{} | {} -c 65536 | wc -c", yes_path(), head_path()),
            "65536",
        ),
        (
            &format!("{} | {} -c 131072 | wc -c", yes_path(), head_path()),
            "131072",
        ),
    ] {
        let output = run_command(script);
        assert!(
            output.status.success(),
            "{script:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.split_whitespace().any(|token| token == expected),
            "{script:?} expected byte count {expected}, got {stdout:?}"
        );
    }
}

/// FD pressure: with low-numbered descriptors occupied, the re-exec protocol
/// and process-substitution handles must still work and deliver intact data.
///
/// This test manipulates the process fd table, so it holds the serial lock
/// for the whole occupy-run-close window and spawns unlocked (the serial
/// helpers would re-lock the same mutex and self-deadlock).
#[test]
fn substitution_survives_fd_pressure() {
    let _serial = serial_guard();
    let mut held = Vec::new();
    for _ in 0..16 {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        held.push(fds[0]);
        held.push(fds[1]);
    }
    let script = "cat <(printf FD-PRESSURE-MARKER); echo SUB:$?";
    let process = spawn_dsh_unlocked(["-c", script], None);
    let output = process
        .wait(DRAIN_TIMEOUT)
        .expect("dsh must complete under fd pressure");
    for fd in held {
        unsafe { libc::close(fd) };
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success() && stdout.contains("FD-PRESSURE-MARKER"),
        "producer lost under fd pressure: stdout={stdout:?} stderr={stderr:?} status={:?}",
        output.status,
    );
}

/// Repeated foreground `<(...)` under a tight child-only FD budget.
///
/// `NOFILE_LIMIT` applies to the dogesh child alone (the parent test process
/// keeps its limits). Each foreground Read substitution must return its
/// retained endpoint and reap its producer synchronously, so per-iteration FD
/// use falls back to baseline. `ITERATIONS > NOFILE_LIMIT` keeps this honest:
/// one leaked descriptor per iteration would exhaust the table before the
/// final marker. `&&` chaining (never `;`) stops a mid-run materialization
/// failure from going green on the trailing marker.
#[test]
fn process_substitution_does_not_exhaust_fd_budget() {
    let _serial = serial_guard();
    const NOFILE_LIMIT: u64 = 96;
    const ITERATIONS: usize = 128;
    let mut script = String::new();
    for _ in 0..ITERATIONS {
        script.push_str("cat <(printf x) > /dev/null && ");
    }
    script.push_str("printf 'FD-RELEASE-OK\\n'");
    let process = spawn_dsh_unlocked_with_nofile_limit(["-c", &script], None, NOFILE_LIMIT);
    let output = process
        .assert_group_drained(Duration::from_secs(60))
        .expect("group must drain after repeated process substitution under fd budget");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success() && stdout.contains("FD-RELEASE-OK"),
        "fd budget exhausted after {ITERATIONS} iterations (limit {NOFILE_LIMIT}): \
         stdout={stdout:?} stderr={stderr:?} status={:?}",
        output.status,
    );
}

/// Explicit concurrency: N shells at once, each with isolated cwd/HOME/XDG,
/// group, and markers. OS-process-level isolation: every shell sees only its
/// own output and every group drains.
#[test]
fn concurrent_shells_are_isolated_and_drained() {
    // The shells are concurrent with each other, but the test as a whole is
    // serialized against the rest of the binary: cross-test concurrency
    // must stay explicit and never accidental.
    let _serial = serial_guard();
    const SHELLS: usize = 4;
    let scripts: Vec<String> = (0..SHELLS)
        .map(|index| {
            format!(
                "echo SHELL-{index}-MARKER | {} a-z A-Z; cat <(printf SHELL-{index}-SUB); echo SHELL-{index}-DONE",
                tr_path()
            )
        })
        .collect();
    // Spawn every shell before waiting for any: the concurrency is real.
    let processes: Vec<_> = scripts
        .iter()
        .map(|script| spawn_dsh_unlocked(["-c", script], None))
        .collect();
    for (index, process) in processes.into_iter().enumerate() {
        let output = process
            .assert_group_drained(Duration::from_secs(10))
            .unwrap_or_else(|err| panic!("shell {index} group did not drain: {err}"));
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            output.status.success(),
            "shell {index} failed: {stdout:?} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!("SHELL-{index}-MARKER")),
            "shell {index} lost its pipeline marker: {stdout:?}"
        );
        assert!(
            stdout.contains(&format!("SHELL-{index}-SUB")),
            "shell {index} lost its substitution marker: {stdout:?}"
        );
        assert!(
            stdout.contains(&format!("SHELL-{index}-DONE")),
            "shell {index} did not complete: {stdout:?}"
        );
        for other in 0..SHELLS {
            if other != index {
                assert!(
                    !stdout.contains(&format!("SHELL-{other}-MARKER")),
                    "shell {index} saw shell {other}'s marker: {stdout:?}"
                );
            }
        }
    }
}

/// Two consecutive top-level async lists must not share a process group.
///
/// Each helper body runs `sh <script>` in the script-file form (allowed by
/// the safety policy without confirmation — `sh -c` would fail closed with
/// exit 130) where the script records its own pgid via `ps`. `ps -o pgid=`
/// works on both Linux (procps) and macOS, and `$$` is expanded by the `sh`
/// running the script — dogesh passes it through untouched. Both helpers
/// are short-lived, so the pgid files exist once the harness observes exit
/// (its pipe EOF follows the inherited write ends); no `sleep`
/// synchronization is needed and the 10s bound only caps a hung shell.
#[test]
fn consecutive_async_jobs_own_distinct_process_groups() {
    let _serial = serial_guard();
    let script = "printf 'ps -o pgid= -p $$ > a.pgid\\n' > pgid_a.sh; \
        printf 'ps -o pgid= -p $$ > b.pgid\\n' > pgid_b.sh; \
        sh pgid_a.sh & sh pgid_b.sh &";
    let process = spawn_dsh_unlocked(["-c", script], None);
    let output = process
        .wait_keep_dirs(Duration::from_secs(10))
        .expect("dsh must exit");
    let stdout = String::from_utf8_lossy(&output.output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.output.stderr).to_string();
    assert!(
        output.output.status.success(),
        "shell failed: stdout={stdout:?} stderr={stderr:?}"
    );

    let a_path = output.workdir.join("a.pgid");
    let b_path = output.workdir.join("b.pgid");

    let a_pgid = std::fs::read_to_string(&a_path)
        .unwrap_or_default()
        .trim()
        .to_string();
    let b_pgid = std::fs::read_to_string(&b_path)
        .unwrap_or_default()
        .trim()
        .to_string();
    assert!(
        !a_pgid.is_empty(),
        "a.pgid is empty: workdir={:?}",
        output.workdir
    );
    assert!(
        !b_pgid.is_empty(),
        "b.pgid is empty: workdir={:?}",
        output.workdir
    );
    assert_ne!(a_pgid, b_pgid, "async jobs shared pgid: {a_pgid}");
}
