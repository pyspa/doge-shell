use serde::Deserialize;
use serde_json::{Value, json};
use shell_words::split;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;
use xdg::BaseDirectories;

use crate::shell_capabilities::{AgentCommandVerdict, ApprovalDecision, ChatToolHost};
use anyhow::Result;
use dsh_types::safety_policy::{string_eval_flag, substitution_construct};

mod capture;
#[cfg(test)]
use capture::CappedCapture;
pub(crate) use capture::kill_process_group;
use capture::render_result;
use capture::run_with_timeout_cancel;

pub(crate) const NAME: &str = "execute";

const EXECUTE_TOOL_CONFIG_FILE: &str = "openai-execute-tool.json";
pub(crate) const EXECUTE_TOOL_ENV_ALLOWLIST: &str = "AI_CHAT_EXECUTE_ALLOWLIST";
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
/// How often the drain wait re-checks whether the readers are still making
/// progress.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Hard ceiling on what one stream may buffer. `render_result` trims to a few
/// kilobytes anyway, so anything beyond this could only cost memory.
const MAX_CAPTURED_BYTES: usize = 1 << 20;

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Run a shell command and return its exit code, stdout, and stderr. Pipes, redirections and `&&` are supported. A command the shell's safety policy considers risky asks the user first, so prefer one clear command over a long chain. Long output is truncated in the middle, so the end of a build or test log is preserved.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command line to run, e.g. `cargo test -p foo 2>&1 | tail -40`."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Directory to run in, relative to the current directory. Defaults to the current directory."
                    },
                    "yield_time_ms": {"type":"integer","minimum":0,"maximum":1000,"description":"Agent tasks: return a managed job handle after this wait."},
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

pub(crate) fn run(arguments: &str, proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for execute tool: {err}"))?;

    let command = parsed
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: execute tool requires `command`".to_string())?
        .trim();

    if command.is_empty() {
        return Err("chat: execute tool command must not be empty".to_string());
    }

    // Before anything parses this line. The shell's parser *evaluates*
    // `$(...)`, `` `...` ``, `(...)` and `<(...)` while building its job list
    // (`shell::parse::parse_command` calls `capture_subshell_stdout`), so
    // handing an unchecked line to the safety evaluation would run the inner
    // pipeline before the user is asked - and again when the command really
    // runs. Pipes, redirection and `&&` are the point of this tool;
    // substitution is not, and refusing it keeps the evaluation a judgement
    // rather than an execution.
    if let Some(construct) = substitution_construct(command) {
        return Err(format!(
            "chat: execute tool does not allow {construct}; run the inner command              separately and use its output"
        ));
    }

    let stages = command_stages(command)?;

    // Still a hard refusal, not a confirmation. A command that hands a string
    // to an interpreter defeats every check below it: whatever the guard reads
    // is the wrapper, not what actually runs.
    for stage in &stages {
        if let Some(flag) = string_eval_flag(&stage.program, &stage.args) {
            return Err(format!(
                "chat: execute tool blocked `{}` because `{flag}` hands it a string to execute; \
                 write the steps as separate commands instead",
                stage.program
            ));
        }

        if let Some(reason) = hidden_code_source(&stage.program, &stage.args) {
            return Err(format!(
                "chat: execute tool blocked `{}` because {reason}; \
                 write the steps as separate commands instead",
                stage.program
            ));
        }
    }

    let cwd = resolve_execution_dir(&parsed, proxy)?;

    if matches!(
        authorize(command, &stages, cwd.as_deref(), proxy)?,
        Authorization::Cancelled
    ) {
        return Ok("Execution cancelled by user.".to_string());
    }

    let timeout_ms = parsed
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);

    if let Some(runtime) = proxy.agent_runtime() {
        let (mut builder, config) = {
            let runtime = runtime.lock();
            crate::agent::sandbox::command(
                command,
                cwd.as_deref().unwrap_or(&runtime.task.root),
                &runtime.task.grant,
                &crate::config_paths::agent_state_dir(),
            )
            .map_err(|e| e.to_string())?
        };
        let env_names = runtime.lock().task.grant.environment.clone();
        for name in env_names {
            if let Some(value) = proxy.get_var(&name) {
                builder.env(name, value);
            }
        }
        let id = runtime
            .lock()
            .jobs
            .start(builder, Duration::from_millis(timeout_ms), config)
            .map_err(|e| e.to_string())?;
        let wait_ms = parsed["yield_time_ms"].as_u64().unwrap_or(1000).min(1000);
        let start = std::time::Instant::now();
        loop {
            if super::super::task_cancelled(proxy) {
                runtime.lock().jobs.cancel(&id).map_err(|e| e.to_string())?;
            }
            let result = runtime
                .lock()
                .jobs
                .snapshot(&id, 0, 4096)
                .map_err(|e| e.to_string())?;
            if result["status"] != "running" || start.elapsed().as_millis() >= wait_ms as u128 {
                return Ok(result.to_string());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let run_result = run_with_timeout_cancel(
        command,
        cwd.as_deref(),
        Duration::from_millis(timeout_ms),
        &|| proxy.is_canceled(),
    )
    .map_err(|err| format!("chat: failed to execute `{command}`: {err}"))?;

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

    let note = if run_result.cancelled {
        Some("command cancelled; output may be partial".into())
    } else {
        match (run_result.timed_out, run_result.drain_incomplete) {
            (true, _) => Some(format!(
                "command exceeded timeout_ms={timeout_ms} and was killed; output below is partial"
            )),
            // Saying nothing here would present a truncated capture as the whole
            // output, which is exactly the mistake the timeout note exists to avoid.
            (false, true) => Some(
                "output capture stopped early; a background process still holds the pipe, so the \
             output below may be incomplete"
                    .to_string(),
            ),
            (false, false) => None,
        }
    };

    Ok(render_result(exit_code, &stdout_text, &stderr_text, note))
}

/// Where the command runs.
///
/// A `cwd` is resolved through the same root check as every other tool path, so
/// the agent cannot step outside the workspace by way of the working directory.
fn resolve_execution_dir(
    parsed: &Value,
    proxy: &mut dyn ChatToolHost,
) -> Result<Option<PathBuf>, String> {
    let Some(requested) = parsed.get("cwd").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    if requested.trim().is_empty() {
        return Ok(None);
    }

    let resolved = super::resolve_tool_path(requested, proxy)?;
    if !resolved.is_dir() {
        return Err(format!(
            "chat: execute tool cwd `{requested}` is not a directory"
        ));
    }
    Ok(Some(resolved))
}

/// One stage of a command line, as the policy checks need it.
struct CommandStage {
    program: String,
    args: Vec<String>,
}

/// Token sequences that end one stage and begin the next.
const STAGE_SEPARATORS: &[&str] = &["|", "||", "&&", ";", "&", "|&"];

/// Split on newlines that are outside quotes.
///
/// `shell_words` treats a newline as ordinary whitespace, so `ls\nbash -c ...`
/// tokenised to a single stage whose program was `ls` - which hid the `bash -c`
/// from the string-eval check and let a bare `ls` allowlist entry wave the
/// whole thing through. A newline *inside* quotes is data, though: splitting
/// `printf 'a\nb\n'` on it leaves two fragments with unbalanced quotes.
fn unquoted_lines(command: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for (index, ch) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\n' | '\r' if !in_single && !in_double => {
                lines.push(&command[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    lines.push(&command[start..]);
    lines
}

/// Split a command line into its stages.
///
/// `shell_words` is a tokenizer, not a parser, so operators arrive as ordinary
/// words. That is enough to answer the two questions asked here - which
/// programs run, and with which arguments - without reimplementing the grammar.
/// The shell's own parser makes the real judgement in `evaluate_agent_command`.
fn command_stages(command: &str) -> Result<Vec<CommandStage>, String> {
    let mut stages = Vec::new();

    for line in unquoted_lines(command) {
        let tokens = split(line).map_err(|err| format!("chat: failed to parse command: {err}"))?;

        let mut current: Vec<String> = Vec::new();
        for token in tokens {
            if STAGE_SEPARATORS.contains(&token.as_str()) {
                push_stage(&mut stages, std::mem::take(&mut current));
            } else {
                current.push(token);
            }
        }
        push_stage(&mut stages, current);
    }

    if stages.is_empty() {
        return Err("chat: execute tool command must specify a program".to_string());
    }
    Ok(stages)
}

/// Programs that run another program, so the one that matters comes after.
///
/// `SafetyGuard` classifies a command by its program name, so `sudo rm -rf ~`
/// used to be classified as `sudo` - which has no checker - and the `rm` rules
/// never ran. Looking through the wrapper is what makes the guard see the
/// command that will actually do the work.
fn push_stage(stages: &mut Vec<CommandStage>, tokens: Vec<String>) {
    // Shared with `SafetyGuard`, so the guard's verdict and this tool's
    // allowlist and skill-script checks look through a wrapper the same way.
    // The local version took the first non-option token as the wrapped program,
    // which for `timeout 5 rm -rf ~` is the timeout value, not `rm`.
    for (program, args) in dsh_types::safety_policy::command_candidates(&tokens) {
        stages.push(CommandStage { program, args });
    }
}

/// Ways of feeding an interpreter code that no check can read.
///
/// `string_eval_flag` covers `sh -c '…'`, but a shell reading its script from
/// standard input (`printf '…' | sh`) carries no flag at all, and `eval` is not
/// an interpreter invocation the flag table describes. Both end with the guard
/// having classified `printf` or `eval` while `sh` runs something else - the
/// same hole the flag refusal exists to close.
fn hidden_code_source(program: &str, args: &[String]) -> Option<&'static str> {
    let name = program_name(program);

    if name == "eval" {
        return Some("`eval` runs a string that cannot be inspected first");
    }

    // A shell with no script operand reads its program from stdin.
    if dsh_types::safety_policy::is_code_execution_command(&name)
        && name != "sudo"
        && args.iter().all(|arg| arg.starts_with('-'))
    {
        return Some("it reads the code to run from standard input");
    }

    // `bash < script.sh` is the same hole with a redirection instead of a pipe:
    // every argument the guard reads is a flag or a filename, the interpreter
    // is classified as itself, and the file it executes is never looked at.
    // `all(starts_with('-'))` above does not hold here because `<` and the
    // filename are ordinary tokens.
    if dsh_types::safety_policy::is_code_execution_command(&name)
        && name != "sudo"
        && args.iter().any(|arg| reads_stdin_from_file(arg))
    {
        return Some("it reads the code to run from a redirected file");
    }

    None
}

/// Whether this token opens standard input from a file.
///
/// `shell_words::split` leaves the redirection as its own token (`<`) when it
/// is spaced and glued to the path (`<script.sh`) when it is not, and a
/// descriptor may lead it (`0<`).
fn reads_stdin_from_file(token: &str) -> bool {
    let rest = token.strip_prefix('0').unwrap_or(token);
    rest.starts_with('<')
}

/// Whether the line redirects output into a file.
///
/// `echo x > ~/.ssh/authorized_keys` is a file write, and the documented
/// contract is that the agent's file writes are confirmed. Redirections are
/// invisible to `SafetyGuard`, which only ever sees programs and arguments.
fn writes_by_redirection(command: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // A backslash is literal inside single quotes, so treating it as an
            // escape there ate the closing quote and left the scanner believing
            // the rest of the line was quoted - `echo 'a\' > out` then looked
            // like it had no redirection at all.
            '\\' if !in_single => {
                chars.next();
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '>' if !in_single && !in_double => {
                // `2>&1` merges descriptors; it opens no file.
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                if chars.peek() != Some(&'&') {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

mod authorize;
use authorize::{Authorization, authorize, program_name};
#[cfg(test)]
use authorize::{command_is_allowlisted, load_allowed_commands};
pub(crate) use authorize::{command_names_any, command_tokens};

#[cfg(test)]
pub(crate) mod tests;
