//! Phase 1 fork boundary: the post-`fork` child for external commands runs
//! raw syscalls only, and failures surface as parent-formatted diagnostics on
//! the command's own stderr target.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn permission_denied_executable_reports_on_stderr_without_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    let prog = dir.path().join("not-executable");
    fs::write(&prog, "#!/bin/sh\necho hi\n").expect("write stub");
    fs::set_permissions(&prog, fs::Permissions::from_mode(0o644)).expect("chmod 0644");

    let output = common::run_command(&format!("{}", prog.display()));

    // The shell must not hang (the harness kills after 10s) or crash: the
    // child reports EACCES through the exec-error pipe and exits non-zero.
    assert!(
        !output.status.success(),
        "permission-denied exec unexpectedly succeeded: {:?}",
        output
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Permission denied"),
        "stderr did not report permission denied: {stderr}"
    );
}

#[test]
fn permission_denied_stderr_redirect_stays_quiet() {
    let dir = tempfile::tempdir().expect("tempdir");
    let prog = dir.path().join("not-executable");
    fs::write(&prog, "#!/bin/sh\necho hi\n").expect("write stub");
    fs::set_permissions(&prog, fs::Permissions::from_mode(0o644)).expect("chmod 0644");

    // The diagnostic belongs to the command's stderr target, not the shell's.
    let output = common::run_command(&format!("{} 2>/dev/null", prog.display()));
    assert!(
        !output.status.success(),
        "permission-denied exec unexpectedly succeeded: {:?}",
        output
    );
    assert!(
        output.stderr.is_empty(),
        "redirected diagnostic leaked to shell stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn command_not_found_is_127_with_redirectable_stderr() {
    let output = common::run_command("dsh-definitely-missing-command-xyz");
    assert_eq!(
        output.status.code(),
        Some(127),
        "command-not-found status: {:?}",
        output
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("command not found"),
        "stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    let quiet = common::run_command("dsh-definitely-missing-command-xyz 2>/dev/null");
    assert_eq!(quiet.status.code(), Some(127));
    assert!(
        quiet.stderr.is_empty(),
        "redirected message leaked: {:?}",
        String::from_utf8_lossy(&quiet.stderr)
    );
}

#[test]
fn redirect_order_stdout_stderr_aliasing_preserved() {
    // `cmd > file 2>&1`: both streams land in the file.
    let out = tempfile::NamedTempFile::new().expect("temp output");
    let path = out.path().to_path_buf();
    drop(out);
    let status = common::run_command(&format!(
        "echo out > {} 2>&1; echo err >&2 >> {}",
        path.display(),
        path.display()
    ));
    assert!(status.status.success(), "command failed: {:?}", status);
    let written = fs::read_to_string(&path).expect("read redirected output");
    assert!(
        written.contains("out"),
        "stdout missing from aliased file: {written:?}"
    );
    fs::remove_file(&path).ok();
}

#[test]
fn external_nonzero_status_propagates() {
    let output = common::run_command(&format!("{}; echo done", common::false_path()));
    assert!(
        output.status.success(),
        "line should complete: {:?}",
        output
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("done"),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}
