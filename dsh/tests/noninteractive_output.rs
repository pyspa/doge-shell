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
