mod common;

use std::fs;
use std::io::Write;

use tempfile::NamedTempFile;

#[test]
fn input_redirect_feeds_command() {
    let mut input = NamedTempFile::new().expect("create temp input");
    writeln!(input, "hello").unwrap();
    writeln!(input, "world").unwrap();

    let cmd = format!("/bin/cat < {}", input.path().display());
    let output = common::run_command(&cmd);

    assert!(output.status.success(), "command failed: {:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\nworld\n");
}

#[test]
fn input_redirect_missing_file_returns_error() {
    let missing_path = std::env::temp_dir().join("dsh_missing_input_test.txt");
    if missing_path.exists() {
        fs::remove_file(&missing_path).ok();
    }
    let cmd = format!("/bin/cat < {}", missing_path.display());
    let output = common::run_command(&cmd);

    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: {:?}",
        output
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to open input redirect file"),
        "stderr did not report missing file: {stderr}"
    );
}

#[test]
fn output_redirect_still_writes_file() {
    let output_file = NamedTempFile::new().expect("create temp output");
    let path = output_file.path().to_path_buf();
    // Drop file handle so shell can write to it
    drop(output_file);

    let cmd = format!("printf 'sample' > {}", path.display());
    let output = common::run_command(&cmd);
    assert!(output.status.success(), "command failed: {:?}", output);

    let written = fs::read_to_string(&path).expect("read redirected output");
    assert_eq!(written, "sample");
    fs::remove_file(path).ok();
}

/// Every redirection on the line takes effect, not just the last one.
///
/// `cmd > out 2> err` used to keep only `2> err` -- the job held a single
/// `Option<Redirect>` that each new redirection overwrote -- so `out` was never
/// created and stdout went to the terminal.
#[test]
fn every_redirect_on_the_line_applies() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("out.txt");
    let err = dir.path().join("err.txt");

    let output = common::run_command(&format!(
        // /etc/hosts rather than /etc/hostname: both Linux and macOS ship it.
        "/bin/ls /nonexistent_dsh_path /etc/hosts > {} 2> {}",
        out.display(),
        err.display()
    ));

    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "stdout should have gone to the file, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        fs::read_to_string(&out)
            .expect("stdout file")
            .contains("hosts"),
        "stdout file should hold the listing"
    );
    assert!(
        !fs::read_to_string(&err).expect("stderr file").is_empty(),
        "stderr file should hold the error"
    );
}

/// `>>` has to create the file. It used to open without `create`, and the
/// failure only surfaced in a spawned task's log, so the append silently
/// vanished.
#[test]
fn append_creates_a_missing_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("append.txt");

    common::run_command(&format!("/bin/echo first >> {}", path.display()));
    common::run_command(&format!("/bin/echo second >> {}", path.display()));

    assert_eq!(
        fs::read_to_string(&path).expect("append file"),
        "first\nsecond\n"
    );
}

/// `2>&1` used to be a parse failure that left `2` behind as an argument, so
/// `ls x 2>&1` ran `ls x 2`.
#[test]
fn stderr_can_be_duplicated_onto_stdout() {
    let output = common::run_command("/bin/ls /nonexistent_dsh_path 2>&1");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("nonexistent_dsh_path"),
        "the error should arrive on stdout, got {stdout:?}"
    );
    assert!(
        !stdout.contains("'2'"),
        "`2` must not be treated as a filename, got {stdout:?}"
    );
}

/// A duplication is applied where it was written, so the two orderings differ --
/// the same rule bash follows.
#[test]
fn redirect_order_decides_where_stderr_goes() {
    let dir = tempfile::tempdir().expect("temp dir");

    // `> file 2>&1`: stderr follows stdout into the file.
    let both = dir.path().join("both.txt");
    let output = common::run_command(&format!(
        "/bin/ls /nonexistent_dsh_path > {} 2>&1",
        both.display()
    ));
    assert!(String::from_utf8_lossy(&output.stderr).trim().is_empty());
    assert!(
        fs::read_to_string(&both)
            .expect("both file")
            .contains("nonexistent_dsh_path"),
        "stderr should have followed stdout into the file"
    );

    // `2>&1 > file`: stderr keeps the destination stdout had *at that point*,
    // so it stays on the terminal while stdout goes to the file.
    let only_out = dir.path().join("only_out.txt");
    let output = common::run_command(&format!(
        "/bin/ls /nonexistent_dsh_path 2>&1 > {}",
        only_out.display()
    ));
    assert!(
        fs::read_to_string(&only_out)
            .expect("stdout file")
            .is_empty(),
        "the error must not reach the file when the dup comes first"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("nonexistent_dsh_path")
            || String::from_utf8_lossy(&output.stdout).contains("nonexistent_dsh_path"),
        "the error should still be reported"
    );
}

/// The duplication belongs to the command it was written on, so it has to reach
/// the pipe rather than the terminal.
#[test]
fn a_duplication_before_a_pipe_feeds_the_pipe() {
    let output = common::run_command("/bin/ls /nonexistent_dsh_path 2>&1 | /usr/bin/wc -l");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "the error line should have been counted by wc"
    );
}

/// A duplication has to survive the alias/variable expansion pass, which
/// re-serializes the line. It used to be dropped there, so the dup worked only
/// on lines that contained no metacharacter at all.
#[test]
fn a_duplication_survives_expansion() {
    let output = common::run_command(
        "/bin/ls /nonexistent_dsh_path $HOME 2>&1 | /usr/bin/grep -c nonexistent_dsh_path",
    );

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "the error should have reached the pipe, not the terminal"
    );
}

/// A redirection-only line creates/truncates its file and reports 0.
#[test]
fn redirect_only_truncates_and_reports_zero() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("out.txt");
    fs::write(&file, "old content that must go").expect("seed file");

    let output = common::run_command(&format!("> {}", file.display()));
    assert!(
        output.status.success(),
        "redirect-only line failed: {:?}",
        output
    );
    assert_eq!(
        fs::read_to_string(&file).expect("truncated file"),
        "",
        "redirect-only `>` must truncate"
    );
}

/// `>>` with no command creates a missing file and never truncates.
#[test]
fn append_only_creates_without_truncating() {
    let dir = tempfile::tempdir().expect("temp dir");
    let fresh = dir.path().join("fresh.txt");
    let output = common::run_command(&format!(">> {}", fresh.display()));
    assert!(
        output.status.success(),
        "append-only create failed: {:?}",
        output
    );
    assert!(fresh.exists(), "append-only `>>` must create the file");

    let kept = dir.path().join("kept.txt");
    fs::write(&kept, "hello\n").expect("seed file");
    let output = common::run_command(&format!(">> {}", kept.display()));
    assert!(
        output.status.success(),
        "append-only on existing file failed: {:?}",
        output
    );
    assert_eq!(
        fs::read_to_string(&kept).expect("kept file"),
        "hello\n",
        "append-only must not truncate"
    );
}

/// `< file` alone checks readability: 0 when present, non-zero when missing.
#[test]
fn input_only_checks_existence() {
    let dir = tempfile::tempdir().expect("temp dir");
    let present = dir.path().join("in.txt");
    fs::write(&present, "hi\n").expect("seed file");

    let output = common::run_command(&format!("< {}", present.display()));
    assert!(
        output.status.success(),
        "present input-only line failed: {:?}",
        output
    );

    let missing = dir.path().join("no-such-input.txt");
    let output = common::run_command(&format!("< {}", missing.display()));
    assert!(
        !output.status.success(),
        "missing input-only line unexpectedly succeeded: {:?}",
        output
    );
}

/// A redirection failure is a command failure: the list continues and
/// `||` recovers while `&&` stays gated.
#[test]
fn redirect_failure_gates_and_or_lists() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");

    let output = common::run_command(&format!("> {} || /bin/echo RECOVERED", missing.display()));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "RECOVERED"),
        "`||` did not run after a failed redirect: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let output = common::run_command(&format!(
        "> {} && /bin/echo SHOULD_NOT_RUN",
        missing.display()
    ));
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("SHOULD_NOT_RUN"),
        "`&&` ran after a failed redirect: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !output.status.success(),
        "failed redirect plus gated `&&` must be non-zero: {:?}",
        output.status.code()
    );
}

/// `FOO=bar > file` applies both: the variable stays in the shell and the
/// file side effect happens.
#[test]
fn assignment_with_redirect_applies_both() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("assigned.txt");
    let output = common::run_command(&format!(
        "FOO=bar > {}; /bin/echo \"[$FOO]\"",
        file.display()
    ));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "[bar]"),
        "assignment did not persist: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(file.exists(), "redirect side effect missing");
}

/// A no-command redirection never leaks into later commands: the file holds
/// only the truncation, and the next command still writes to the terminal.
#[test]
fn redirect_only_does_not_leak_into_later_commands() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("trunc.txt");
    let output = common::run_command(&format!("> {}; /bin/echo AFTER", file.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.trim() == "AFTER"),
        "the next command lost its stdout: {stdout:?}"
    );
    assert_eq!(
        fs::read_to_string(&file).expect("truncated file"),
        "",
        "later output leaked into the redirected file"
    );
}

/// A substitution inside the redirect target feeds the no-command status:
///
/// `> $(helper-that-prints-a-path-and-exits-7)` creates the file and the
/// simple command reports 7.
#[test]
fn redirect_target_substitution_status_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("via_subst.txt");
    let helper = dir.path().join("mkpath.sh");
    fs::write(
        &helper,
        format!("#!/bin/sh\nprintf '%s' \"{}\"\nexit 7\n", target.display()),
    )
    .expect("write helper");
    let mut perms = fs::metadata(&helper).expect("stat helper").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&helper, perms).expect("chmod helper");

    let output = common::run_command(&format!(
        "> $({}); /bin/echo \"status=$?\"",
        helper.display()
    ));
    assert!(target.exists(), "redirect target was not created");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "status=7"),
        "expected status=7 from the target substitution, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Only the three standard descriptors are tracked, so any other source would
/// name one of the shell's own files -- its history database or config -- and
/// hand the child a writable duplicate.
#[test]
fn duplicating_an_untracked_descriptor_is_refused() {
    let output = common::run_command("/bin/ls / 2>&3");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("bad file descriptor"),
        "expected a refusal, got {stderr:?}"
    );
}

/// Every output-redirection operator fails the same way: the command reports
/// 1 and the shell continues. (`>` and `<` are pinned by contracts; `>>`,
/// `2>`, and `&>` are covered here.)
#[test]
fn every_output_operator_failure_continues_the_shell() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir");

    for (operator, category) in [
        (">>", "failed to open redirect file"),
        ("2>", "failed to create redirect file"),
        ("&>", "failed to create redirect file"),
    ] {
        let script = format!(
            "/bin/echo hi {operator} {}/out; echo ST:$?",
            missing.display()
        );
        let output = common::run_command(&script);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.lines().any(|line| line.trim() == "ST:1"),
            "{operator}: the shell did not continue with status 1: {stdout:?} {stderr:?}"
        );
        assert!(
            stderr.contains(category),
            "{operator}: missing diagnostic category: {stderr:?}"
        );
    }
}

/// A runnable redirection failure is reported exactly once: the evaluator
/// owns the diagnostic, and no lower layer prints it too.
#[test]
fn runnable_redirect_failure_reports_exactly_one_diagnostic() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");
    let output = common::run_command(&format!("/bin/echo hi > {}; echo AFTER", missing.display()));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        stderr.matches("failed to create redirect file").count(),
        1,
        "diagnostic must be reported exactly once, got: {stderr:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "AFTER"),
        "the shell did not continue: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// A background job whose redirection fails before spawn is never registered:
/// the status is non-zero and the shell continues.
#[test]
fn background_redirect_failure_is_not_a_job() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");
    let output = common::run_command(&format!(
        "/bin/echo hi > {} & echo BG:$?",
        missing.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.lines().any(|line| line.trim() == "BG:1"),
        "background redirect failure must report non-zero: {stdout:?} {stderr:?}"
    );
    assert!(
        stderr.contains("failed to create redirect file"),
        "missing diagnostic: {stderr:?}"
    );
}

/// A redirection failure through the `|>` capture path is a command failure:
/// the diagnostic travels the capture stderr and the shell continues.
#[test]
fn capture_pipe_redirect_failure_is_a_command_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");
    let output = common::run_command(&format!(
        "{} > {} |>; echo AFTER:$?",
        common::true_path(),
        missing.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.lines().any(|line| line.trim() == "AFTER:1"),
        "capture path did not continue with status 1: {stdout:?} {stderr:?}"
    );
    assert!(
        stderr.contains("failed to create redirect file"),
        "diagnostic missed the capture stderr: {stderr:?}"
    );
}

/// A redirection failure through the `|:` struct-pipe path is a command
/// failure as well: no Lisp evaluation, status 1, shell continues.
#[test]
fn struct_pipe_redirect_failure_is_a_command_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");
    let output = common::run_command(&format!(
        "echo '[{{\"a\":1}}]' > {} |: (json-parse $_); echo AFTER:$?",
        missing.display(),
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.lines().any(|line| line.trim() == "AFTER:1"),
        "struct-pipe path did not continue with status 1: {stdout:?} {stderr:?}"
    );
    assert!(
        stderr.contains("failed to create redirect file"),
        "diagnostic missed the struct-pipe stderr: {stderr:?}"
    );
}

/// A second regular builtin (`uuid`) fails the same way as `help`: this pins
/// the fix to the shared launch path instead of one command.
#[test]
fn second_regular_builtin_redirect_failure_is_a_command_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");
    let output = common::run_command(&format!(
        "uuid > {} || /bin/echo RECOVERED",
        missing.display()
    ));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.lines().any(|line| line.trim() == "RECOVERED"),
        "`||` did not recover a builtin redirect failure: {stdout:?} {stderr:?}"
    );
    assert!(
        stderr.contains("failed to create redirect file"),
        "missing diagnostic: {stderr:?}"
    );
}

/// Dozens of consecutive failures keep the shell usable: pipes still connect
/// and fresh redirects still land. (A portable behavioral proxy for fd-leak
/// freedom: a leaked pipe end per failure would break the pipe or the
/// redirect below.)
#[test]
fn many_consecutive_failures_keep_pipes_and_redirects_working() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-dir").join("out");
    let ok = dir.path().join("ok.txt");
    let mut script = String::new();
    for _ in 0..30 {
        script.push_str(&format!(
            "{} > {}; ",
            common::true_path(),
            missing.display()
        ));
    }
    script.push_str(&format!(
        "/bin/echo piped | {} a-z A-Z; echo PIPE:$?; /bin/echo data > {}; /bin/cat {}",
        common::tr_path(),
        ok.display(),
        ok.display()
    ));
    let output = common::run_command(&script);
    let stdout = String::from_utf8_lossy(&output.stdout);

    for expected in ["PIPED", "PIPE:0", "data"] {
        assert!(
            stdout.contains(expected),
            "shell degraded after repeated failures, missing {expected:?}: {stdout:?}"
        );
    }
}
