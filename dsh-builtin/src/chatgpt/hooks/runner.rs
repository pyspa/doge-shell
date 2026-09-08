//! Running one hook process.
//!
//! The event is written to the hook's stdin and the answer is read from its
//! stdout. Nothing goes through a shell and nothing goes in `argv`: the payload
//! carries text the model chose and text the user typed, and `argv` is visible
//! to every other user through `ps`.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::config::{HOOK_DEPTH_ENV, HookDefinition};

/// Ceiling on what one hook may say. A hook that streams megabytes is broken,
/// and its answer is a small JSON object either way.
const MAX_CAPTURE_BYTES: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Exit code that means "stop this", so a one-line hook needs no JSON.
const DENY_EXIT_CODE: i32 = 2;

/// What a hook said, before it is turned into a decision.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookResponse {
    /// `"deny"`, `"ask"`, or absent. There is deliberately no `"allow"`:
    /// a hook can stop something or ask about it, never permit it.
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Shown to the user, never to the model.
    #[serde(default)]
    pub message: Option<String>,
    /// Appended to what the model sees.
    #[serde(default)]
    pub additional_context: Option<String>,
}

#[derive(Debug)]
pub(crate) enum HookRun {
    Answered(HookResponse),
    /// The hook did not produce a usable answer. What that means is the
    /// caller's decision, and it depends on the event.
    Failed(String),
}

pub(crate) fn run_hook(
    hook: &HookDefinition,
    payload: &str,
    env: &[(String, String)],
    cwd: &Path,
) -> HookRun {
    let mut builder = Command::new(&hook.command[0]);
    builder
        .args(&hook.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout takes the whole tree down instead of
        // orphaning whatever the hook spawned.
        .process_group(0)
        .env(HOOK_DEPTH_ENV, "1");

    if cwd.is_dir() {
        builder.current_dir(cwd);
    }
    for (key, value) in env {
        builder.env(key, value);
    }

    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            return HookRun::Failed(format!("could not start `{}`: {err}", hook.command[0]));
        }
    };

    // The payload has to be written from another thread. Writing it inline
    // blocks as soon as the pipe buffer fills, and the hook cannot drain it
    // while we are not reading its stdout - a deadlock that only shows up once
    // a payload gets big enough, which is to say in production.
    let stdin = child.stdin.take();
    let payload = payload.to_string();
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(payload.as_bytes());
            // Dropping the handle is the EOF the hook is waiting for.
        }
    });

    let stdout = capture(child.stdout.take());
    let stderr = capture(child.stderr.take());

    let deadline = Instant::now() + Duration::from_millis(hook.timeout_ms());
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => {
                return HookRun::Failed(format!("could not wait for `{}`: {err}", hook.id));
            }
        }

        if Instant::now() >= deadline {
            crate::chatgpt::tool::execute::kill_process_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }

        std::thread::sleep(POLL_INTERVAL);
    };

    let _ = writer.join();
    // The child is gone, so EOF has either arrived or never will; take what the
    // readers have either way.
    wait_for_eof(&[&stdout, &stderr], Duration::from_millis(200));
    let stdout = stdout.text();
    let stderr = stderr.text();

    let Some(status) = status else {
        return HookRun::Failed(format!("timed out after {}ms", hook.timeout_ms()));
    };

    interpret(status.code(), &stdout, &stderr)
}

fn interpret(code: Option<i32>, stdout: &str, stderr: &str) -> HookRun {
    // `exit 2` beats stdout so that `grep -q dangerous && exit 2` is a complete
    // hook. It can only ever restrict, so allowing the shorthand costs nothing.
    if code == Some(DENY_EXIT_CODE) {
        let reason = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("blocked by hook")
            .to_string();
        return HookRun::Answered(HookResponse {
            decision: Some("deny".to_string()),
            reason: Some(reason),
            ..HookResponse::default()
        });
    }

    if code != Some(0) {
        let detail = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("no output");
        return match code {
            Some(code) => HookRun::Failed(format!("exited with {code}: {detail}")),
            None => HookRun::Failed(format!("was killed by a signal: {detail}")),
        };
    }

    let trimmed = stdout.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        // Printing something that is not JSON is how a logging hook behaves.
        return HookRun::Answered(HookResponse::default());
    }

    match serde_json::from_str::<HookResponse>(trimmed) {
        Ok(response) => match response.decision.as_deref() {
            None | Some("deny") | Some("ask") => HookRun::Answered(response),
            Some("allow") => HookRun::Failed(
                "answered `allow`, which hooks cannot do: a hook may stop an action or ask about it, never permit one".to_string(),
            ),
            Some(other) => HookRun::Failed(format!("answered with unknown decision `{other}`")),
        },
        Err(err) => HookRun::Failed(format!("printed JSON that could not be read: {err}")),
    }
}

struct Capture {
    buffer: Arc<Mutex<Vec<u8>>>,
    at_eof: Arc<AtomicBool>,
}

impl Capture {
    fn text(&self) -> String {
        let buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

fn capture<R>(pipe: Option<R>) -> Capture
where
    R: Read + Send + 'static,
{
    let capture = Capture {
        buffer: Arc::new(Mutex::new(Vec::new())),
        at_eof: Arc::new(AtomicBool::new(false)),
    };
    let buffer = Arc::clone(&capture.buffer);
    let at_eof = Arc::clone(&capture.at_eof);

    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0_u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut buffer = buffer
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        // Keep the head: a JSON answer starts at byte zero, and
                        // anything past the cap is not part of one.
                        let room = MAX_CAPTURE_BYTES.saturating_sub(buffer.len());
                        if room > 0 {
                            buffer.extend_from_slice(&chunk[..read.min(room)]);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        at_eof.store(true, Ordering::Release);
    });

    capture
}

fn wait_for_eof(captures: &[&Capture], grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if captures
            .iter()
            .all(|capture| capture.at_eof.load(Ordering::Acquire))
        {
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn script(dir: &TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("hook.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn definition(command: Vec<String>, timeout_ms: u64) -> HookDefinition {
        let json = serde_json::json!({
            "id": "test",
            "events": ["pre-tool-use"],
            "command": command,
            "timeout_ms": timeout_ms,
        });
        serde_json::from_value(json).unwrap()
    }

    fn run(dir: &TempDir, hook: &HookDefinition, payload: &str) -> HookRun {
        run_hook(hook, payload, &[], dir.path())
    }

    #[test]
    fn stdout_json_deny_becomes_a_deny_decision() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, r#"echo '{"decision":"deny","reason":"nope"}'"#);
        let hook = definition(vec![path.display().to_string()], 5000);

        match run(&dir, &hook, "{}") {
            HookRun::Answered(response) => {
                assert_eq!(response.decision.as_deref(), Some("deny"));
                assert_eq!(response.reason.as_deref(), Some("nope"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exit_code_two_denies_with_stderr_as_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, "echo 'writes outside the repo' >&2\nexit 2");
        let hook = definition(vec![path.display().to_string()], 5000);

        match run(&dir, &hook, "{}") {
            HookRun::Answered(response) => {
                assert_eq!(response.decision.as_deref(), Some("deny"));
                assert_eq!(response.reason.as_deref(), Some("writes outside the repo"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn non_json_stdout_on_success_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, "echo logged it");
        let hook = definition(vec![path.display().to_string()], 5000);

        match run(&dir, &hook, "{}") {
            HookRun::Answered(response) => assert!(response.decision.is_none()),
            other => panic!("{other:?}"),
        }
    }

    /// Accepting `allow` would make a hook a fourth way to grant permission.
    #[test]
    fn allow_decision_in_json_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, r#"echo '{"decision":"allow"}'"#);
        let hook = definition(vec![path.display().to_string()], 5000);

        match run(&dir, &hook, "{}") {
            HookRun::Failed(err) => assert!(err.contains("never permit"), "{err}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unreadable_answer_is_a_failure_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, r#"echo '{"decision":'"#);
        let hook = definition(vec![path.display().to_string()], 5000);

        match run(&dir, &hook, "{}") {
            HookRun::Failed(err) => assert!(err.contains("could not be read"), "{err}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn payload_arrives_on_stdin_not_in_argv() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        let path = script(
            &dir,
            &format!(
                "cat > {}\necho \"argv:$*\" >> {}",
                out.display(),
                out.display()
            ),
        );
        let hook = definition(vec![path.display().to_string()], 5000);

        run(&dir, &hook, r#"{"event":"pre-tool-use"}"#);

        let seen = std::fs::read_to_string(&out).unwrap();
        assert!(seen.contains(r#"{"event":"pre-tool-use"}"#), "{seen}");
        assert!(
            seen.contains("argv:\n") || seen.trim_end().ends_with("argv:"),
            "{seen}"
        );
    }

    /// The writer thread exists for this: a payload larger than a pipe buffer
    /// plus a hook that talks back used to deadlock both sides.
    #[test]
    fn large_payload_and_large_output_do_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        // Reads its whole stdin only after writing a lot of its own output.
        let path = script(&dir, "yes abcdefghij | head -c 200000\ncat > /dev/null");
        let hook = definition(vec![path.display().to_string()], 10_000);
        let payload = "x".repeat(512 * 1024);

        match run(&dir, &hook, &payload) {
            HookRun::Answered(response) => assert!(response.decision.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, "sleep 30");
        let hook = definition(vec![path.display().to_string()], 200);

        let started = Instant::now();
        match run(&dir, &hook, "{}") {
            HookRun::Failed(err) => assert!(err.contains("timed out"), "{err}"),
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_program_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let hook = definition(vec!["/definitely/not/here".to_string()], 500);

        match run(&dir, &hook, "{}") {
            HookRun::Failed(err) => assert!(err.contains("could not start"), "{err}"),
            other => panic!("{other:?}"),
        }
    }

    /// Arguments reach the program as written. If they went through a shell,
    /// `$HOME` would expand and `;` would start a second command.
    #[test]
    fn command_is_not_run_through_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("argv.txt");
        let path = script(&dir, &format!("printf '%s' \"$1\" > {}", out.display()));
        let hook = definition(
            vec![path.display().to_string(), "$HOME; rm -rf /".to_string()],
            5000,
        );

        run(&dir, &hook, "{}");

        assert_eq!(std::fs::read_to_string(&out).unwrap(), "$HOME; rm -rf /");
    }

    #[test]
    fn the_depth_flag_is_set_on_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("depth.txt");
        let path = script(
            &dir,
            &format!("printf '%s' \"$DSH_HOOK_DEPTH\" > {}", out.display()),
        );
        let hook = definition(vec![path.display().to_string()], 5000);

        run(&dir, &hook, "{}");

        assert_eq!(std::fs::read_to_string(&out).unwrap(), "1");
    }
}
