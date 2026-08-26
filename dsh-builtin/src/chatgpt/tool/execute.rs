use serde::Deserialize;
use serde_json::{Value, json};
use shell_words::split;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use xdg::BaseDirectories;

use crate::ShellProxy;
use anyhow::Result;

pub(crate) const NAME: &str = "execute";

const EXECUTE_TOOL_CONFIG_FILE: &str = "openai-execute-tool.json";
const EXECUTE_TOOL_ENV_ALLOWLIST: &str = "AI_CHAT_EXECUTE_ALLOWLIST";
const EXECUTE_TOOL_CONFIG_OVERRIDE_ENV: &str = "DSH_EXECUTE_TOOL_CONFIG";
const CONFIG_DIR_PREFIX: &str = "dsh";

/// Wall-clock budget for a single `execute` call when the caller does not ask
/// for one. Without a timeout a build or a dev server wedges the whole shell.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const TIMEOUT_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Per-stream budget applied before the global tool-output cap, so a chatty
/// stdout can never push stderr out of the result.
const MAX_STREAM_CHARS: usize = 3072;
/// Floor for that budget when the serialized result still does not fit.
const MIN_STREAM_CHARS: usize = 256;
/// How long to wait for the output readers once the child is gone.
///
/// A killed child that left a grandchild behind keeps the write end of the pipe
/// open, so waiting for EOF can never finish; that would defeat the timeout
/// this whole path exists for.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Execute an allowlisted command directly (without shell evaluation) and return exit code, stdout, and stderr. Long output is truncated in the middle, so the end of a build or test log is preserved. Configure allowlist in ~/.config/dsh/openai-execute-tool.json or AI_CHAT_EXECUTE_ALLOWLIST.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command line to execute. Shell operators are not allowed."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "description": "Kill the command after this many milliseconds. Defaults to 120000."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for execute tool: {err}"))?;

    let command = parsed
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: execute tool requires `command`".to_string())?;

    if command.trim().is_empty() {
        return Err("chat: execute tool command must not be empty".to_string());
    }

    let tokens = parse_command_tokens(command)?;
    if contains_shell_expression(&tokens) {
        return Err(
            "chat: execute tool does not allow shell operators or substitutions; pass plain command and arguments only".to_string(),
        );
    }

    let program = tokens[0].clone();
    let args = tokens[1..].to_vec();

    if invokes_string_eval(&program, &args) {
        return Err(format!(
            "chat: execute tool blocked `{}` because string-eval flags (-c/-e/-command) are not allowed",
            program
        ));
    }

    let allowlist = load_allowed_commands(proxy.list_execute_allowlist())?;
    let allowed = command_is_allowlisted(&program, &args, &allowlist);
    let is_skill_script = is_skill_script_program(&program, proxy)?;

    if !allowed && !is_skill_script {
        return Err(format!(
            "chat: execute tool command `{}` from request `{}` is not permitted. Allowed entries: {} (or scripts in skills directory)",
            program,
            command.trim(),
            allowlist.join(", ")
        ));
    }

    if is_skill_script {
        let confirm_msg = format!(
            "AI wants to execute skill script: `{}`. \r\nProceed?",
            command.trim()
        );
        if !proxy
            .confirm_action(&confirm_msg)
            .map_err(|e: anyhow::Error| e.to_string())?
        {
            return Ok("Execution cancelled by user.".to_string());
        }
    }

    let timeout_ms = parsed
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);

    let run_result = run_with_timeout(&program, &args, Duration::from_millis(timeout_ms))
        .map_err(|err| format!("chat: failed to execute `{}`: {err}", command.trim()))?;

    let stdout_text = String::from_utf8_lossy(&run_result.stdout).to_string();
    let stderr_text = String::from_utf8_lossy(&run_result.stderr).to_string();

    if !stdout_text.is_empty() {
        let mut stdout = io::stdout();
        stdout
            .write_all(stdout_text.as_bytes())
            .map_err(|e| e.to_string())?;
    }

    if !stderr_text.is_empty() {
        let mut stderr = io::stderr();
        stderr
            .write_all(stderr_text.as_bytes())
            .map_err(|e| e.to_string())?;
    }

    let exit_code = run_result
        .status
        .and_then(|status| status.code())
        .unwrap_or(-1);

    let note = run_result.timed_out.then(|| {
        format!("command exceeded timeout_ms={timeout_ms} and was killed; output below is partial")
    });

    Ok(render_result(exit_code, &stdout_text, &stderr_text, note))
}

/// Serialize the result so that it survives the global tool-output cap intact.
///
/// The per-stream budgets bound the *raw* text, but JSON escaping can double or
/// sextuple it. Handing an oversized object to the shared truncator produced a
/// middle-cut, unparseable JSON document.
fn render_result(exit_code: i32, stdout: &str, stderr: &str, note: Option<String>) -> String {
    let mut budget = MAX_STREAM_CHARS;

    loop {
        let mut result = json!({
            "exit_code": exit_code,
            "stdout": dsh_openai::turn::truncate_middle(stdout, budget),
            "stderr": dsh_openai::turn::truncate_middle(stderr, budget),
        });

        if let Some(note) = &note
            && let Some(map) = result.as_object_mut()
        {
            map.insert("note".into(), json!(note));
        }

        let rendered = result.to_string();
        if rendered.len() <= super::MAX_OUTPUT_LENGTH || budget <= MIN_STREAM_CHARS {
            return rendered;
        }

        budget /= 2;
    }
}

struct CapturedRun {
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

/// Drain a child pipe on its own thread.
///
/// Polling only for exit deadlocks as soon as the child fills a pipe buffer, so
/// both streams have to be read while we wait. The result arrives over a
/// channel rather than a join handle so that the caller can stop waiting.
fn drain_pipe<R>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buffer);
        }
        let _ = tx.send(buffer);
    });
    rx
}

fn run_with_timeout(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<CapturedRun, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout can take the whole tree down instead
        // of orphaning whatever the command spawned.
        .process_group(0)
        .spawn()
        .map_err(|err| err.to_string())?;

    let stdout_reader = drain_pipe(child.stdout.take());
    let stderr_reader = drain_pipe(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => return Err(err.to_string()),
        }

        if Instant::now() >= deadline {
            kill_process_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }

        std::thread::sleep(TIMEOUT_POLL_INTERVAL);
    };

    // Bounded: a surviving grandchild still holds the write end of the pipe, so
    // EOF may never arrive. Give up on the tail rather than on the shell. Both
    // readers share one deadline so the wait cannot add up.
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    let stdout = recv_until(&stdout_reader, drain_deadline);
    let stderr = recv_until(&stderr_reader, drain_deadline);

    Ok(CapturedRun {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

fn recv_until(reader: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    reader.recv_timeout(remaining).unwrap_or_default()
}

/// Signal the whole group the child leads, so background grandchildren die too.
fn kill_process_group(child: &std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // `process_group(0)` made the child its own group leader, so pgid == pid.
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
}

fn parse_command_tokens(command: &str) -> Result<Vec<String>, String> {
    let tokens = split(command).map_err(|err| format!("chat: failed to parse command: {err}"))?;
    if tokens.is_empty() {
        Err("chat: execute tool command must specify a program".to_string())
    } else {
        Ok(tokens)
    }
}

#[cfg(test)]
fn extract_program_name(command: &str) -> Result<String, String> {
    parse_command_tokens(command).map(|tokens| tokens[0].clone())
}

fn contains_shell_expression(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        token.contains("$(")
            || token.contains('`')
            || token.contains('\n')
            || token
                .chars()
                .any(|ch| matches!(ch, '|' | '&' | ';' | '>' | '<'))
    })
}

fn program_name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or(program)
        .to_string()
}

fn is_path_qualified(program: &str) -> bool {
    program.contains('/') || program.contains('\\') || Path::new(program).is_absolute()
}

fn invokes_string_eval(program: &str, args: &[String]) -> bool {
    let name = program_name(program);
    match name.as_str() {
        "sh" | "bash" | "zsh" | "fish" | "ksh" => {
            args.iter().any(|arg| arg == "-c" || arg == "-lc")
        }
        "python" | "python3" | "perl" | "ruby" | "node" => {
            args.iter().any(|arg| arg == "-c" || arg == "-e")
        }
        "pwsh" | "powershell" => args
            .iter()
            .any(|arg| arg.eq_ignore_ascii_case("-c") || arg.eq_ignore_ascii_case("-command")),
        _ => false,
    }
}

fn allowlist_program_matches(entry_program: &str, program: &str) -> bool {
    let entry_name = program_name(entry_program);
    let target_name = program_name(program);

    if is_path_qualified(entry_program) {
        entry_program == program
    } else if is_path_qualified(program) {
        false
    } else {
        entry_name == target_name
    }
}

fn allowlist_entry_matches(entry: &str, program: &str, args: &[String]) -> bool {
    let entry_tokens = match split(entry) {
        Ok(tokens) if !tokens.is_empty() => tokens,
        _ => return false,
    };

    if !allowlist_program_matches(&entry_tokens[0], program) {
        return false;
    }

    if entry_tokens.len() == 1 {
        true
    } else {
        entry_tokens[1..] == args[..]
    }
}

fn command_is_allowlisted(program: &str, args: &[String], allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|entry| allowlist_entry_matches(entry, program, args))
}

fn is_skill_script_program(program: &str, proxy: &mut dyn ShellProxy) -> Result<bool, String> {
    if !program.contains('/') && !Path::new(program).is_absolute() {
        return Ok(false);
    }

    let resolved_program = match super::resolve_tool_path(program, proxy) {
        Ok(path) => path,
        Err(_) => return Ok(false),
    };

    let skills_root = std::fs::canonicalize(super::tool_skills_dir())
        .unwrap_or_else(|_| super::normalize_path(&super::tool_skills_dir()));

    Ok(resolved_program.starts_with(skills_root))
}

fn load_allowed_commands(runtime_allowed: Vec<String>) -> Result<Vec<String>, String> {
    if let Some(mut commands) = read_allowlist_from_env() {
        commands.sort();
        commands.dedup();
        return Ok(commands);
    }

    let mut allowlist = runtime_allowed;

    if let Some(config_path) = resolve_allowlist_path()?
        && let Some(mut file_allowlist) = read_allowlist_from_file(&config_path)?
    {
        allowlist.append(&mut file_allowlist);
    }

    allowlist.sort();
    allowlist.dedup();
    Ok(allowlist)
}

fn read_allowlist_from_file(path: &PathBuf) -> Result<Option<Vec<String>>, String> {
    let contents = fs::read_to_string(path).map_err(|err| {
        format!(
            "chat: failed to read execute tool config {}: {err}",
            path.display()
        )
    })?;

    if contents.trim().is_empty() {
        return Ok(Some(Vec::new()));
    }

    #[derive(Deserialize)]
    struct ExecuteAllowlist {
        #[serde(default)]
        allowed_commands: Vec<String>,
    }

    let raw: ExecuteAllowlist = serde_json::from_str(&contents)
        .map_err(|err| format!("chat: failed to parse {} as JSON: {err}", path.display()))?;

    Ok(Some(
        raw.allowed_commands
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    ))
}

fn read_allowlist_from_env() -> Option<Vec<String>> {
    let raw = env::var(EXECUTE_TOOL_ENV_ALLOWLIST).ok()?;
    let entries: Vec<String> = raw
        .split([',', '\n'])
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();

    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn resolve_allowlist_path() -> Result<Option<PathBuf>, String> {
    if let Ok(path) = env::var(EXECUTE_TOOL_CONFIG_OVERRIDE_ENV) {
        return Ok(Some(PathBuf::from(path)));
    }

    let xdg_dirs = BaseDirectories::with_prefix(CONFIG_DIR_PREFIX);

    Ok(xdg_dirs.find_config_file(EXECUTE_TOOL_CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use tempfile::tempdir;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    use crate::test_support::TestShellProxy;
    type TestProxy = TestShellProxy;

    #[test]
    fn extract_program_name_returns_first_token() {
        assert_eq!(extract_program_name("ls -la").unwrap(), "ls");
        assert_eq!(extract_program_name("git status").unwrap(), "git");
    }

    #[test]
    fn load_allowlist_prefers_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls,git\ncat");
        assert_eq!(
            load_allowed_commands(vec![]).unwrap(),
            vec!["cat", "git", "ls"]
        );
    }

    #[test]
    fn load_allowlist_reads_config_file() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("allow.json");
        let contents = json!({ "allowed_commands": ["cargo"] }).to_string();
        std::fs::write(&config_path, contents).unwrap();

        let _env_guard = EnvGuard::set(
            EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
            config_path.to_str().unwrap(),
        );
        let _allow_env = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        assert_eq!(load_allowed_commands(vec![]).unwrap(), vec!["cargo"]);
    }

    #[test]
    fn load_allowlist_merges_runtime_entries() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("allow.json");
        let contents = json!({ "allowed_commands": ["cargo"] }).to_string();
        std::fs::write(&config_path, contents).unwrap();

        let _env_guard = EnvGuard::set(
            EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
            config_path.to_str().unwrap(),
        );
        let _allow_env = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");

        let allowlist = load_allowed_commands(vec!["ls".to_string(), "cargo".to_string()]).unwrap();
        assert_eq!(allowlist, vec!["cargo", "ls"]);
    }

    #[test]
    fn run_reports_full_command_on_disallowed_program() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };
        let result = run("{\"command\":\"cat README.md\"}", &mut proxy);
        let err = result.expect_err("command should be rejected");
        assert!(err.contains("`cat`"));
        assert!(err.contains("`cat README.md`"));
    }

    #[test]
    fn run_rejects_shell_expression() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let result = run("{\"command\":\"ls ; rm -rf /tmp/x\"}", &mut proxy);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("does not allow shell operators")
        );

        let result = run("{\"command\":\"ls $(pwd)\"}", &mut proxy);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("does not allow shell operators")
        );
    }

    #[test]
    fn run_rejects_string_eval_flags() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "bash");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["bash".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let result = run("{\"command\":\"bash -lc 'echo hi'\"}", &mut proxy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("string-eval flags"));
    }

    #[test]
    fn basename_allowlist_does_not_match_path_qualified_program() {
        assert!(command_is_allowlisted(
            "git",
            &["status".to_string()],
            &["git".to_string()]
        ));
        assert!(!command_is_allowlisted(
            "/tmp/git",
            &["status".to_string()],
            &["git".to_string()]
        ));
        assert!(command_is_allowlisted(
            "/tmp/git",
            &["status".to_string()],
            &["/tmp/git".to_string()]
        ));
    }

    #[test]
    fn run_rejects_path_qualified_spoofing_for_basename_allowlist() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "git");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["git".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let result = run("{\"command\":\"/tmp/git status\"}", &mut proxy);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is not permitted"));
    }

    #[test]
    fn run_skips_confirmation_for_allowlisted_command() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
        let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: true,
            ..TestProxy::default()
        };
        let result = run("{\"command\":\"ls -la\"}", &mut proxy);
        assert!(result.is_ok());
        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn run_requires_confirmation_for_skill_script() {
        let _lock = ENV_LOCK.lock().unwrap();
        let config_root = tempdir().unwrap();
        let skills_dir = config_root.path().join("dsh/skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let script_path = skills_dir.join("script.sh");
        std::fs::write(&script_path, "#!/usr/bin/env bash\necho hello\n").unwrap();

        let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

        let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            execute_allowlist: vec![],
            current_dir: std::env::current_dir().unwrap(),
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let command = format!("{{\"command\":\"{}\"}}", script_path.to_string_lossy());
        let result = run(&command, &mut proxy);

        assert_eq!(result.unwrap(), "Execution cancelled by user.".to_string());
        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn run_kills_a_command_that_exceeds_its_timeout() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "sleep");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["sleep".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let started = Instant::now();
        let result = run("{\"command\":\"sleep 30\",\"timeout_ms\":1000}", &mut proxy).unwrap();
        let elapsed = started.elapsed();

        assert!(result.contains("exceeded timeout_ms=1000"), "{result}");
        assert!(result.contains("\"exit_code\":-1"), "{result}");
        assert!(
            elapsed < Duration::from_secs(20),
            "did not return early: {elapsed:?}"
        );
    }

    #[test]
    fn run_returns_stderr_alongside_exit_code() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let result = run(
            "{\"command\":\"ls /definitely-missing-path-for-dsh-test\"}",
            &mut proxy,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert_ne!(parsed["exit_code"].as_i64().unwrap(), 0);
        assert!(
            !parsed["stderr"].as_str().unwrap().is_empty(),
            "stderr was dropped: {result}"
        );
    }

    #[test]
    fn render_result_stays_parseable_when_escaping_inflates_it() {
        // Regression: the per-stream caps bound raw text, but JSON escaping can
        // multiply it, and the shared truncator then cut the middle out of the
        // serialized object.
        let noisy = "\u{1b}[31mboom\n".repeat(2000);
        let rendered = render_result(1, &noisy, &noisy, None);

        assert!(
            rendered.len() <= crate::chatgpt::tool::MAX_OUTPUT_LENGTH,
            "result is {} bytes",
            rendered.len()
        );
        let parsed: Value = serde_json::from_str(&rendered).expect("result must be valid JSON");
        assert_eq!(parsed["exit_code"], 1);
        assert!(!parsed["stdout"].as_str().unwrap().is_empty());
        assert!(!parsed["stderr"].as_str().unwrap().is_empty());
    }

    #[test]
    fn render_result_reports_a_timeout_note() {
        let rendered = render_result(-1, "out", "", Some("killed".to_string()));
        let parsed: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["note"], "killed");
    }

    #[test]
    fn run_returns_even_while_a_grandchild_holds_the_pipe() {
        // A script that backgrounds a process keeps the write end of stdout
        // open after it exits, so waiting for EOF would hang the shell.
        let _lock = ENV_LOCK.lock().unwrap();

        let dir = tempdir().unwrap();
        let script = dir.path().join("spawner.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 5 &\necho started\n").unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let allowlist = script.to_string_lossy().to_string();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, &allowlist);
        let mut proxy = TestProxy {
            execute_allowlist: vec![allowlist.clone()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let started = Instant::now();
        let result = run(&format!("{{\"command\":\"{allowlist}\"}}"), &mut proxy).unwrap();
        let elapsed = started.elapsed();

        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exit_code"].as_i64().unwrap(), 0);
        assert!(
            elapsed < DRAIN_GRACE + Duration::from_secs(1),
            "waited for the grandchild: {elapsed:?}"
        );
    }

    #[test]
    fn a_large_stdout_does_not_evict_stderr() {
        // Regression: the whole result JSON used to be cut from the front, so a
        // chatty stdout dropped the error message the model needed.
        let _lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");

        let dir = tempdir().unwrap();
        for index in 0..300 {
            std::fs::write(dir.path().join(format!("file-{index:0>40}")), b"x").unwrap();
        }

        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        let command = format!(
            "{{\"command\":\"ls {} /definitely-missing-path-for-dsh-test\"}}",
            dir.path().display()
        );
        let result = run(&command, &mut proxy).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        let stdout = parsed["stdout"].as_str().unwrap();
        let stderr = parsed["stderr"].as_str().unwrap();

        assert!(stdout.len() > MAX_STREAM_CHARS / 2, "stdout was empty");
        assert!(stdout.contains("truncated"), "stdout was not truncated");
        assert!(!stderr.is_empty(), "stderr was dropped: {result}");
        assert_ne!(parsed["exit_code"].as_i64().unwrap(), 0);
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                unsafe {
                    env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }
}
