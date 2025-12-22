use std::io::Write;
use std::process::{Command, Stdio};

fn run_dsh(lines: &[&str]) -> std::process::Output {
    let dsh_path = env!("CARGO_BIN_EXE_dsh");

    let mut child = Command::new(dsh_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn dsh");

    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        for line in lines {
            writeln!(stdin, "{line}").unwrap();
        }
        writeln!(stdin, "exit").unwrap();
    }

    child.wait_with_output().expect("Failed to read output")
}

#[test]
fn logical_and_or_short_circuit_works() {
    let output = run_dsh(&[
        "false && echo SHOULD_NOT_PRINT_AND",
        "true || echo SHOULD_NOT_PRINT_OR",
        "false && echo SHOULD_NOT_PRINT_A || echo SHOULD_PRINT_B",
        "true && echo SHOULD_PRINT_C || echo SHOULD_NOT_PRINT_D",
    ]);

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("SHOULD_NOT_PRINT_AND"),
        "Unexpected output for && short-circuit. stdout:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("SHOULD_NOT_PRINT_OR"),
        "Unexpected output for || short-circuit. stdout:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("SHOULD_NOT_PRINT_A"),
        "Unexpected output for a && b || c (a=false). stdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("SHOULD_PRINT_B"),
        "Missing expected output for a && b || c (a=false). stdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("SHOULD_PRINT_C"),
        "Missing expected output for a && b || c (a=true). stdout:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("SHOULD_NOT_PRINT_D"),
        "Unexpected output for a && b || c (a=true). stdout:\n{}",
        stdout
    );
}
