use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir() -> std::path::PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX_EPOCH")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsh-special-pipes-{ts}"));
    std::fs::create_dir_all(&dir).expect("failed to create temp test dir");
    dir
}

fn spawn_dsh_with_temp_xdg() -> std::process::Child {
    let dsh_path = env!("CARGO_BIN_EXE_dsh");
    let dir = unique_test_dir();

    Command::new(dsh_path)
        .env("XDG_STATE_HOME", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("XDG_CONFIG_HOME", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn dsh")
}

#[test]
fn capture_suffix_updates_output_history() {
    let mut child = spawn_dsh_with_temp_xdg();

    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        writeln!(stdin, "echo hello-capture |>").unwrap();
        writeln!(stdin, "| cat").unwrap();
        writeln!(stdin, "exit").unwrap();
    }

    let output = child.wait_with_output().expect("Failed to read stdout");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let occurrences = stdout.matches("hello-capture").count();
    assert!(
        occurrences >= 2,
        "Expected captured output to be reusable via smart pipe. Output:\n{}",
        stdout
    );
}

#[test]
fn struct_pipe_chains_lisp_expressions() {
    let mut child = spawn_dsh_with_temp_xdg();

    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        writeln!(
            stdin,
            "echo '[{{\"a\":1}},{{\"a\":2}}]' |: (json-parse $_) |: (table-count $_)"
        )
        .unwrap();
        writeln!(stdin, "exit").unwrap();
    }

    let output = child.wait_with_output().expect("Failed to read stdout");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.trim() == "2"),
        "Expected struct pipe to print table count result. Output:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("[{\"a\":1},{\"a\":2}]"),
        "Raw command stdout should not be printed for struct pipe. Output:\n{}",
        stdout
    );
}
