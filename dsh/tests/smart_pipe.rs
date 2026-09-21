mod common;

#[test]
fn test_smart_pipe_via_stdin() {
    let tr = common::tr_path();
    let output = common::run_interactive(&["echo hello-smart-pipe", &format!("| {tr} a-z A-Z")]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Previous stdout must flow through the Smart Pipe into the downstream
    // transform: a mere replay of the cached text would leave it lowercase,
    // so only the transformed form proves real pipe data-flow.
    assert!(
        stdout.contains("HELLO-SMART-PIPE"),
        "Smart Pipe downstream transform missing. Output:\n{}",
        stdout
    );
}

/// A synthetic source feeding a no-command downstream is a formal two-stage
/// pipeline (`SyntheticSource | NoCommand`), never a collapsed stage: the
/// helper applies the assignment in isolation, the parent keeps its value,
/// and the pipeline status is 0.
#[test]
fn test_smart_pipe_into_no_command_stage_is_isolated() {
    let output = common::run_interactive(&[
        "FOO=sp_parent",
        "echo hello-sp-source",
        "| FOO=child_sp",
        "echo SPSTATUS:$?",
        "echo SPFOO:$FOO",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("SPSTATUS:0"),
        "synthetic-source plus no-command pipeline must succeed. Output:\n{}",
        stdout
    );
    assert!(
        stdout.lines().any(|line| line.trim() == "SPFOO:sp_parent"),
        "the pipeline assignment leaked into the parent: {stdout:?}"
    );
    assert!(
        !stderr.contains("no command"),
        "no no-command refusal diagnostic may remain. stderr:\n{}",
        stderr
    );
}

/// Large synthetic source (> typical 64KiB pipe capacity) must not deadlock:
/// the source helper and the downstream consumer run concurrently, and an
/// early-exiting consumer (`head -c 1`) terminates the pipeline cleanly with
/// no hang, no internal error, and no leaked helper.
///
/// The declarative contract harness cannot carry a >64KiB stdout through its
/// wait-then-read pipes, so this drains stdout/stderr concurrently while the
/// child runs.
#[test]
fn test_smart_pipe_large_source_no_deadlock() {
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use wait_timeout::ChildExt;

    let yes = common::yes_path();
    let head = common::head_path();
    let temp = tempfile::TempDir::new().expect("tempdir");
    let workdir = temp.path().join("work");
    std::fs::create_dir_all(&workdir).expect("workdir");
    let replay = workdir.join("replay.txt");
    let script = format!("{yes} | {head} -c 131072\n| {head} -c 1 > replay.txt\nexit\n");

    let mut child = Command::new(env!("CARGO_BIN_EXE_dogesh"))
        .current_dir(&workdir)
        .env("XDG_STATE_HOME", temp.path())
        .env("XDG_DATA_HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn dogesh");

    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(script.as_bytes())
        .expect("write script");

    // Drain concurrently so a >64KiB previous output never blocks the child
    // on a full harness pipe.
    let mut stdout_handle = child.stdout.take().expect("stdout");
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout_handle.read_to_end(&mut buf).ok();
        buf
    });
    let mut stderr_handle = child.stderr.take().expect("stderr");
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stderr_handle.read_to_end(&mut buf).ok();
        buf
    });

    let status = match child.wait_timeout(Duration::from_secs(15)).expect("wait") {
        Some(status) => status,
        None => {
            let pgid = nix::unistd::Pid::from_raw(child.id() as i32);
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
            let _ = child.wait();
            panic!("large Smart Pipe source deadlocked (timed out after 15s)");
        }
    };
    assert!(status.success(), "smart pipe pipeline failed: {status:?}");
    let stdout = stdout_thread.join().expect("stdout thread");
    let stderr = stderr_thread.join().expect("stderr thread");
    let stdout_text = String::from_utf8_lossy(&stdout);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        !stderr_text.contains("Broken pipe") && !stderr_text.contains("internal exec failed"),
        "source EPIPE must stay silent. stderr:\n{stderr_text}"
    );
    let _ = stdout_text;
    let content = std::fs::read_to_string(&replay)
        .unwrap_or_else(|_| panic!("replay.txt missing in {}", workdir.display()));
    assert_eq!(content, "y", "replay must be the first source byte");
}
