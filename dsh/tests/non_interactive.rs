mod common;

use std::time::Duration;

#[test]
fn pipe_input_joins_a_final_backslash_newline_at_eof() {
    let output = common::run_dsh_with_input(
        [] as [&str; 0],
        Some(b"printf '%s' final\\\n"),
        Duration::from_secs(5),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"final");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn command_mode_exits_without_waiting_for_interactive_prewarm() {
    let output = common::run_dsh(["-c", "true"], Duration::from_secs(2));
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lisp_mode_exits_after_printing_result() {
    let output = common::run_dsh(["-l", "(+ 1 2)"], Duration::from_secs(2));
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "3");
}

#[test]
fn command_mode_preserves_notebook_setup() {
    let notebook = tempfile::NamedTempFile::new().expect("create notebook path");
    let notebook_path = notebook.path().display().to_string();
    let output = common::run_dsh(
        ["--notebook", notebook_path.as_str(), "-c", "true"],
        Duration::from_secs(2),
    );

    assert!(output.status.success(), "dsh command failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Notebook Mode Active."),
        "combined command/notebook mode skipped notebook setup: {output:?}"
    );
}

#[test]
fn lisp_mode_preserves_notebook_setup() {
    let notebook = tempfile::NamedTempFile::new().expect("create notebook path");
    let notebook_path = notebook.path().display().to_string();
    let output = common::run_dsh(
        ["--notebook", notebook_path.as_str(), "-l", "(+ 1 2)"],
        Duration::from_secs(2),
    );

    assert!(output.status.success(), "dsh lisp failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Notebook Mode Active."));
    assert!(stdout.lines().any(|line| line.trim() == "3"));
}

#[test]
fn timeout_kills_descendants_that_hold_output_pipes() {
    let started = std::time::Instant::now();
    let result = std::panic::catch_unwind(|| {
        common::run_dsh(["-c", "/bin/sleep 30"], Duration::from_millis(100))
    });

    assert!(result.is_err(), "expected the helper to report a timeout");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "timeout handling waited for a descendant process"
    );
}

#[test]
fn notebook_play_runs_confirmed_scripts_with_the_callers_streams() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("book.md"),
        "> ```sh\n> exit 19\n> ```\n\n```bash\nwords=(first \"$LABEL\")\nif read -r text; then\n  printf '%s: %s\\n' \"${words[1]}\" \"$text\"\nfi\n```\n\n~~~sh\nprintf 'from sh\\n' >&2\nexit 23\n~~~\n\n```\ntouch unexpected\n```\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("answers"), "y\nblock input\n\ny\n").unwrap();
    let command = format!(
        "cd '{}'; export LABEL=recorded; notebook-play book.md < answers > replay.out 2> replay.err",
        dir.path().display()
    );
    let output = common::run_command(&command);
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    let stdout = std::fs::read_to_string(dir.path().join("replay.out")).unwrap();
    assert!(
        stdout.lines().any(|line| line == "recorded: block input"),
        "{stdout:?}"
    );
    let stderr = std::fs::read_to_string(dir.path().join("replay.err")).unwrap();
    assert!(stderr.lines().any(|line| line == "from sh"), "{stderr:?}");
    assert!(stderr.contains("block 2"), "{stderr:?}");
    assert!(!dir.path().join("unexpected").exists());
}

#[test]
fn notebook_play_requires_confirmation_before_each_block() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("book.md"),
        "```\ntouch skipped\n```\n```sh\ntouch unconfirmed\n```\n",
    )
    .unwrap();
    let command = format!("cd '{}'; notebook-play book.md", dir.path().display());
    let output =
        common::run_dsh_with_input(["-c", &command], Some(b"n\n "), Duration::from_secs(5));
    assert!(output.status.success(), "{output:?}");
    assert!(!dir.path().join("skipped").exists());
    assert!(!dir.path().join("unconfirmed").exists());
}

#[test]
fn notebook_startup_rejects_missing_parent_and_invalid_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = dir.path().join("invalid.md");
    std::fs::write(&invalid, [0xff]).unwrap();
    let marker = dir.path().join("executed");
    let command = format!("touch '{}'", marker.display());
    for path in [dir.path().join("missing/book.md"), invalid.clone()] {
        let path = path.to_str().unwrap();
        let output = common::run_dsh(["--notebook", path, "-c", &command], Duration::from_secs(5));
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(path),
            "{output:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Active"));
        assert!(!marker.exists());
    }
    assert_eq!(std::fs::read(&invalid).unwrap(), [0xff]);
    let output = common::run_command(&format!("notebook-play '{}'", invalid.display()));
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(invalid.to_str().unwrap()));
}

#[test]
fn notebook_append_failure_reports_path_and_preserves_command_status() {
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path().join("new.md");
    let old = dir.path().join("recorded.md");
    let input = format!(
        "mv '{0}' '{1}'; mkdir '{0}'\necho still-running; exit 17\n",
        book.display(),
        old.display()
    );
    let output = common::run_dsh_with_input(
        ["--notebook", book.to_str().unwrap()],
        Some(input.as_bytes()),
        Duration::from_secs(5),
    );
    assert_eq!(output.status.code(), Some(17), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("append") && stderr.contains(book.to_str().unwrap()),
        "{stderr:?}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("still-running"));
    assert!(std::fs::read_to_string(old).unwrap().contains("mkdir"));
}
