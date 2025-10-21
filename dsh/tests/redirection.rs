use std::fs;
use std::io::Write;
use std::process::Command;

use tempfile::NamedTempFile;

fn run_dsh(command: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dsh"))
        .args(["-c", command])
        .output()
        .expect("failed to execute dsh")
}

#[test]
fn input_redirect_feeds_command() {
    let mut input = NamedTempFile::new().expect("create temp input");
    writeln!(input, "hello").unwrap();
    writeln!(input, "world").unwrap();

    let cmd = format!("/bin/cat < {}", input.path().display());
    let output = run_dsh(&cmd);

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
    let output = run_dsh(&cmd);

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
    let output = run_dsh(&cmd);
    assert!(output.status.success(), "command failed: {:?}", output);

    let written = fs::read_to_string(&path).expect("read redirected output");
    assert_eq!(written, "sample");
    fs::remove_file(path).ok();
}
