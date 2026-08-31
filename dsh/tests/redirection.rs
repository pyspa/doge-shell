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
