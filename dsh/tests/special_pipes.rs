mod common;

#[test]
fn capture_suffix_updates_output_history() {
    let output = common::run_interactive(&["echo hello-capture |>", "| cat"]);
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
    let output = common::run_interactive(&[
        "echo '[{\"a\":1},{\"a\":2}]' |: (json-parse $_) |: (table-count $_)",
    ]);
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
