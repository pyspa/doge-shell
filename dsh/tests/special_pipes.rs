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

/// A malformed `|:` DSL stage used to be a parse-time `Result::Err` that
/// discarded every job already built for the same `;`-joined line -- so one
/// typo in a later command's `|:` silently ate the earlier, perfectly valid
/// commands too. It must now fail only its own job, at run time.
#[test]
fn a_malformed_struct_pipe_does_not_take_down_earlier_commands_on_the_same_line() {
    let output =
        common::run_interactive(&["echo line-one ; echo line-two |: frobnicate ; echo line-three"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("line-one"),
        "Expected the command before the bad |: to still run. Output:\n{}",
        stdout
    );
    assert!(
        stdout.contains("line-three"),
        "Expected the command after the bad |: to still run. Output:\n{}",
        stdout
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Struct pipe error") && stderr.contains("frobnicate"),
        "Expected the bad |: stage to still report an error. Stderr:\n{}",
        stderr
    );
}
