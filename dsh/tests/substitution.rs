//! Command substitution has to come back as a value, not as terminal output.
//!
//! `$(...)` used to `fork()` from under the multi-threaded Tokio runtime, which
//! aborted the child inside Tokio's IO driver whenever stdin was a terminal, and
//! it handed the substitution pipe over as a bare `ctx.outfile`, which the
//! non-interactive auto-capture path overwrote: the caller read an empty string
//! while the inner command's output appeared on the terminal.

mod common;

use common::run_command;
use std::os::unix::fs::PermissionsExt;

fn stdout_of(command: &str) -> String {
    let output = run_command(command);
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn write_executable_script(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).expect("write helper script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat helper script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod helper script");
    path
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
    let skipped = stdout_of(&format!(
        "/bin/echo [$({} && /bin/echo X)]",
        common::false_path()
    ));
    assert!(
        !skipped.contains('X'),
        "`&&` ran the second command anyway: {skipped:?}"
    );

    let run = stdout_of(&format!(
        "/bin/echo [$({} && /bin/echo X)]",
        common::true_path()
    ));
    assert!(
        run.contains('X'),
        "`&&` skipped the second command: {run:?}"
    );

    let short_circuited = stdout_of(&format!(
        "/bin/echo [$({} || /bin/echo X)]",
        common::true_path()
    ));
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

    // `/tmp` is a symlink to `/private/tmp` on macOS, and the shell reports the
    // resolved path, so compare against whatever `/tmp` resolves to here.
    let tmp = std::fs::canonicalize("/tmp").expect("canonical /tmp");
    let tmp = tmp.to_string_lossy();

    assert!(
        stdout.lines().any(|line| line.trim() == tmp),
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

/// A bare `$(...)` with no command name reports the helper's real status:
/// `$(false)` is 1, `$(true)` is 0 -- not a blanket success.
#[test]
fn bare_substitution_reports_helper_status() {
    let failed = run_command(&format!("$({})", common::false_path()));
    assert_eq!(
        failed.status.code(),
        Some(1),
        "bare $(false) must exit 1. stderr:\n{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    let ok = run_command(&format!("$({})", common::true_path()));
    assert_eq!(
        ok.status.code(),
        Some(0),
        "bare $(true) must exit 0. stderr:\n{}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

/// Same, for an arbitrary non-zero status through the helper exit code.
#[test]
fn bare_substitution_reports_arbitrary_status() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let exit7 = write_executable_script(&dir, "exit7.sh", "#!/bin/sh\nexit 7\n");
    let output = run_command(&format!("$({})", exit7.display()));
    assert_eq!(
        output.status.code(),
        Some(7),
        "bare $(exit-7) must exit 7. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The no-command status gates `&&` / `||` like any other command status.
#[test]
fn bare_substitution_gates_and_or_lists() {
    let gated = stdout_of(&format!(
        "$({}) && /bin/echo SHOULD_NOT_RUN",
        common::false_path()
    ));
    assert!(
        !gated.contains("SHOULD_NOT_RUN"),
        "`&&` ran after a failing substitution: {gated:?}"
    );
    let recovered = stdout_of(&format!(
        "$({}) || /bin/echo EXPECTED",
        common::false_path()
    ));
    assert!(
        recovered.lines().any(|line| line.trim() == "EXPECTED"),
        "`||` did not run after a failing substitution: {recovered:?}"
    );
}

/// With several substitutions, the *last* one decides -- not the first.
#[test]
fn last_command_substitution_status_wins() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let exit7 = write_executable_script(&dir, "exit7b.sh", "#!/bin/sh\nexit 7\n");
    // Both bodies print nothing; only their statuses differ.
    let output = run_command(&format!(
        "$({}) $({})",
        common::false_path(),
        exit7.display()
    ));
    assert_eq!(
        output.status.code(),
        Some(7),
        "last status (7) must win. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_command(&format!(
        "$({}) $({})",
        exit7.display(),
        common::false_path()
    ));
    assert_eq!(
        output.status.code(),
        Some(1),
        "last status (1) must win. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A surviving command reports its own status: `echo $(false)` is 0.
#[test]
fn surviving_command_ignores_substitution_status() {
    let output = run_command(&format!("/bin/echo $({})", common::false_path()));
    assert!(
        output.status.success(),
        "echo with a failing substitution must succeed: {:?}",
        output.status.code()
    );
}

/// The isolated helper evaluator shares the no-command semantics: gating and
/// `$?` inside `$(...)` behave exactly as on the top level.
#[test]
fn helper_evaluator_shares_no_command_semantics() {
    let gated = stdout_of(&format!(
        "/bin/echo $({} && /bin/echo SHOULD_NOT_RUN)",
        common::false_path()
    ));
    assert!(
        !gated.contains("SHOULD_NOT_RUN"),
        "helper && branch must not run: {gated:?}"
    );
    let status = stdout_of(&format!(
        "/bin/echo $({}; /bin/echo status=$?)",
        common::false_path()
    ));
    assert!(
        status.lines().any(|line| line.trim() == "status=1"),
        "helper must publish the no-command status: {status:?}"
    );
}

/// A signal death inside the substitution surfaces as 128+signal end to end.
#[test]
fn bare_substitution_reports_signal_status() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let sigterm = write_executable_script(&dir, "sigterm_sub.sh", "#!/bin/sh\nkill -TERM $$\n");
    let stdout = stdout_of(&format!(
        "$({}); /bin/echo \"status=$?\"",
        sigterm.display()
    ));
    assert!(
        stdout.lines().any(|line| line.trim() == "status=143"),
        "expected status=143 after SIGTERM substitution, got {stdout:?}"
    );
}

/// Expanding the body must not cost it its operators.
///
/// Uses a fixed `aaa` input so the expectation does not depend on `$HOME`:
/// neither `/home/runner` nor `/Users/runner` contains an `a`, so the old
/// `$HOME | tr a A` assertion could never see an `A` on CI runners.
#[test]
fn an_expanded_substitution_body_keeps_its_pipeline() {
    let stdout = stdout_of(&format!(
        "/bin/echo $(/bin/echo aaa | {} a A)",
        common::tr_path()
    ));
    assert!(
        stdout.lines().any(|line| line.trim() == "AAA"),
        "the pipeline inside the substitution did not run: {stdout:?}"
    );
    assert!(
        !stdout.contains("tr"),
        "the pipe was lost and `tr` became an argument: {stdout:?}"
    );
}

/// `>(...)` via tempfile rendezvous: deterministic, no stdout race.
#[test]
fn output_substitution_writes_to_tempfile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out.txt");
    let output = run_command(&format!("printf hello > >(cat > {})", out.display()));
    assert!(
        output.status.success(),
        "outer must succeed: {:?}",
        output.status.code()
    );
    // Command-mode shell releases the consumer without killing it; the
    // helper may outlive `dogesh -c`. Poll bounded for the drained file.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(data) = std::fs::read(&out)
            && data == b"hello"
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "consumer never wrote hello"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `>(...)` as a plain argument: `tee >(cat > file)`.
#[test]
fn output_substitution_as_argument_writes_to_tempfile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("tee_out.txt");
    let output = run_command(&format!(
        "printf hello | tee >(cat > {}) > /dev/null",
        out.display()
    ));
    assert!(
        output.status.success(),
        "tee must succeed: {:?}",
        output.status.code()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(data) = std::fs::read(&out)
            && (data == b"hello\n" || data == b"hello")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tee consumer never wrote"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Helper status never becomes outer status: `true > >(false)` is 0.
#[test]
fn output_consumer_failure_does_not_change_outer_status() {
    let output = run_command(&format!(
        "{} > >({})",
        common::true_path(),
        common::false_path()
    ));
    assert_eq!(
        output.status.code(),
        Some(0),
        "outer true must stay 0 even though consumer is false"
    );
}

/// Large output through `>(...)` keeps tail bytes.
#[test]
fn output_substitution_keeps_large_payload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("large.txt");
    // 300 KiB via `head -c` from /dev/zero piped through tr? Use yes+head
    // for portability: `yes ABC | head -c 300000 > >(cat > file)`.
    // `head -c` byte count keeps the harness pipe small (we check the file).
    let output = run_command(&format!(
        "{} | {} -c 300000 > >(cat > {})",
        common::yes_path(),
        common::head_path(),
        out.display()
    ));
    assert!(
        output.status.success(),
        "large producer must succeed: {:?}",
        output.status.code()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(data) = std::fs::read(&out)
            && data.len() == 300_000
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "large consumer never completed"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Nested `>(...)` inside `$(...)` keeps direction and payload.
#[test]
fn nested_output_substitution_inside_command_substitution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("nested.txt");
    let stdout = stdout_of(&format!(
        "echo $(printf payload > >(cat > {}); echo done)",
        out.display()
    ));
    assert!(
        stdout.lines().any(|line| line.trim() == "done"),
        "outer capture must see done in {stdout:?}"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(data) = std::fs::read(&out)
            && data == b"payload"
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "nested consumer never wrote payload"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
