//! Structured Lisp command results through real non-TTY script invocations.
mod common;
use common::process::{spawn_dsh_unlocked, spawn_dsh_unlocked_with_nofile_limit};
use std::time::Duration;

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn multimegabyte_stdout_and_stderr_are_drained_and_retained_exactly() {
    let _serial = common::serial_guard();
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("large-output.sh");
    let chunk = "日本語 ".repeat(64);
    std::fs::write(&helper,format!("payload='{chunk}'\ni=0\nwhile [ \"$i\" -lt 4096 ]; do printf '%s' \"$payload\"; printf '%s' \"$payload\" >&2; i=$((i+1)); done\n")).unwrap();
    let program = format!(
        "(let ((r (command-result {} {}))) (print (hash_get r \"stdout\")) (print (hash_get r \"stderr\")) (hash_get r \"exit-code\"))",
        quoted(common::sh_path()),
        quoted(helper.to_str().unwrap())
    );
    let output = spawn_dsh_unlocked(["-l", &program], None)
        .assert_group_drained(Duration::from_secs(15))
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let captured = chunk.repeat(4096);
    assert_eq!(
        output.stdout,
        format!("{captured}\n{captured}\n0\n").as_bytes()
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn repeated_results_release_child_and_pipe_ownership_under_fd_limit() {
    let _serial = common::serial_guard();
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("count.sh");
    let counter = root.path().join("counter");
    std::fs::write(&helper, "printf x >> \"$1\"\nprintf out\nprintf err >&2\n").unwrap();
    let form = format!(
        "(hash_get (command-result {} {} {}) \"exit-code\")\n",
        quoted(common::sh_path()),
        quoted(helper.to_str().unwrap()),
        quoted(counter.to_str().unwrap())
    );
    let program = form.repeat(128);
    let output = spawn_dsh_unlocked_with_nofile_limit(["-l", &program], None, 96)
        .assert_group_drained(Duration::from_secs(15))
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"0\n");
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read(counter).unwrap(), vec![b'x'; 128]);
}

#[test]
fn file_script_has_closed_child_stdin_and_structured_failure_data() {
    let _serial = common::serial_guard();
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("stdin.sh");
    let script = root.path().join("result.lisp");
    std::fs::write(
        &helper,
        "if read line; then printf unexpected; else printf eof; fi\nprintf err >&2\nexit 4\n",
    )
    .unwrap();
    std::fs::write(&script,format!("(let ((r (command-result {} {}))) (print (hash_get r \"stdout\")) (print (hash_get r \"stderr\")) (hash_get r \"exit-code\"))",quoted(common::sh_path()),quoted(helper.to_str().unwrap()))).unwrap();
    let output = spawn_dsh_unlocked(["lisp", script.to_str().unwrap()], Some("unused input\n"))
        .assert_group_drained(Duration::from_secs(10))
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"eof\nerr\n4\n");
    assert!(output.stderr.is_empty());
}
