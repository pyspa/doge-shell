//! Noninteractive output remains byte-exact across capture and redirection.
mod common;

use common::contract::expand_tokens;
use common::process::{DEFAULT_CASE_TIMEOUT, spawn_dsh_unlocked};

#[test]
fn external_output_preserves_bytes_and_streams() {
    let _guard = common::serial_guard();
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("{{SH}} -c 'printf abc'", b"abc", b""),
        ("{{SH}} -c 'printf err >&2'", b"", b"err"),
        ("{{SH}} -c 'printf out; printf err >&2'", b"out", b"err"),
        (
            "{{SH}} -c 'printf \"a\\000\\377\\nlast\"'",
            b"a\0\xff\nlast",
            b"",
        ),
        ("{{SH}} -c 'printf \"e\\000\\377\" >&2'", b"", b"e\0\xff"),
        (
            "{{SH}} -c 'printf out; printf err >&2' 2>&1",
            b"outerr",
            b"",
        ),
        (
            "{{SH}} -c 'printf out; printf err >&2' > result; cat result",
            b"out",
            b"err",
        ),
        ("{{SH}} -c 'printf abc' | cat", b"abc", b""),
        (
            "{{SH}} -c 'printf out; printf err >&2' & wait",
            b"out",
            b"err",
        ),
    ];
    for (script, stdout, stderr) in cases {
        // Use a file-backed helper so the default safety policy does not
        // require confirmation for a nested string-evaluation command.
        let (source, suffix) = script
            .strip_prefix("{{SH}} -c '")
            .unwrap()
            .rsplit_once('\'')
            .unwrap();
        let script = expand_tokens(&format!(
            "printf '%s' '{source}' > emit.sh; {{{{SH}}}} emit.sh{suffix}"
        ));
        let output = spawn_dsh_unlocked(["-c", &script], None)
            .wait(DEFAULT_CASE_TIMEOUT)
            .expect("bounded command completion");
        assert!(output.status.success(), "{script}: {output:?}");
        assert_eq!(output.stdout, *stdout, "stdout for {script}");
        assert_eq!(output.stderr, *stderr, "stderr for {script}");
    }
}

fn large_alias() -> (String, String) {
    let value = "x".repeat(100_000);
    (
        format!("alias probe={value}; "),
        format!("probe='{value}'\n"),
    )
}

#[test]
fn parent_builtin_large_output_and_history_are_exact() {
    let _guard = common::serial_guard();
    let (setup, expected) = large_alias();
    let script = format!("{setup}alias probe; printf '%s' \"$OUT\"");
    let output = spawn_dsh_unlocked(["-c", &script], None)
        .wait(DEFAULT_CASE_TIMEOUT)
        .expect("parent builtin must not block on its own capture pipe");
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, expected.repeat(2).as_bytes());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn parent_builtin_large_output_preserves_redirect_and_pipeline() {
    let _guard = common::serial_guard();
    let (setup, expected) = large_alias();
    for suffix in ["alias probe > result; cat result", "alias probe | cat"] {
        let script = format!("{setup}{suffix}");
        let output = spawn_dsh_unlocked(["-c", &script], None)
            .wait(DEFAULT_CASE_TIMEOUT)
            .expect("redirected and pipeline builtin output must complete");
        assert!(output.status.success(), "{suffix}: {:?}", output.stderr);
        assert_eq!(output.stdout, expected.as_bytes(), "{suffix}");
        assert!(output.stderr.is_empty(), "{suffix}: {:?}", output.stderr);
    }
}

#[test]
fn repeated_parent_builtin_capture_releases_fds_and_keeps_parent_state() {
    let _guard = common::serial_guard();
    let (setup, expected) = large_alias();
    let script = format!(
        "{setup}{}export CAPTURE_KEEP=kept; cd ..; alias after=parent; alias after; echo $CAPTURE_KEEP; pwd",
        "alias probe; ".repeat(80),
    );
    let child = common::process::spawn_dsh_unlocked_with_nofile_limit(["-c", &script], None, 64);
    let expected_cwd = child.workdir().parent().unwrap().canonicalize().unwrap();
    let output = child
        .wait(DEFAULT_CASE_TIMEOUT)
        .expect("repeated captures must release readers and wake descriptors");
    assert!(output.status.success(), "{:?}", output.stderr);
    let expected = format!(
        "{}after='parent'\nkept\n{}\n",
        expected.repeat(80),
        expected_cwd.display(),
    );
    assert_eq!(output.stdout, expected.as_bytes());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}
