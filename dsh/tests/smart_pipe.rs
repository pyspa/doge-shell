use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn test_smart_pipe_via_stdin() {
    let dsh_path = env!("CARGO_BIN_EXE_dsh");

    // We launch dsh in interactive mode (or just reading from stdin)
    let mut child = Command::new(dsh_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn dsh");

    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        // Command 1: echo hello (captured)
        // Command 2: | cat (uses smart pipe)
        // exit
        writeln!(stdin, "echo hello-smart-pipe").unwrap();
        // Give it a tiny bit of time? No, stdin is buffered.
        // We hope dsh reads line by line.
        writeln!(stdin, "| cat").unwrap();
        writeln!(stdin, "exit").unwrap();
    }

    let output = child.wait_with_output().expect("Failed to read stdout");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // We expect "hello-smart-pipe" to appear twice correctly if smart pipe works:
    // Once from the first echo
    // Once from the "| cat" which replays it
    // Note: dsh might output prompts, so simple contains check is best.

    let occurrences = stdout.matches("hello-smart-pipe").count();
    // Depending on how dsh prints output (it might not print plain output if non-interactive? but we are using piped stdout)
    // Usually echo prints to stdout.

    // Actually, `dsh` might behave differently if not a TTY.
    // But `echo` is a builtin or external command that prints to stdout.

    // We expect at least one occurrence from the first command.
    // And if smart logic works, the second command `| cat` becomes `__dsh_print_last_stdout | cat`
    // which prints the cached output.

    assert!(
        occurrences >= 2,
        "Expected output to be repeated. Output:\n{}",
        stdout
    );
}
