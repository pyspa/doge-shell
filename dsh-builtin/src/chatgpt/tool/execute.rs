//! The `execute` tool: its schema, the checks that run before a command line
//! becomes a process, and the two ways it is run.
//!
//! The order here is the contract. A line is refused outright (command
//! substitution, string-as-code, hidden code sources), then `authorize`
//! puts it through the shell's safety guard and allowlists, and only then does
//! it reach [`capture::shell_command`]. Nothing below `authorize` may widen
//! what runs; see `ai/safety.md`.
//!
//! Both entry points run the command as a managed job
//! ([`crate::agent::jobs::AgentJobs`]). A task's jobs belong to its
//! `AgentRuntime`; an interactive turn's belong to
//! [`crate::chatgpt::jobs`] and can outlive the turn that started them.
use serde::Deserialize;
use serde_json::{Value, json};
use shell_words::split;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use xdg::BaseDirectories;

use crate::shell_capabilities::{AgentCommandVerdict, ApprovalDecision, ChatToolHost};
use anyhow::Result;
use dsh_types::safety_policy::{string_eval_flag, substitution_construct};

mod authorize;
mod capture;
mod jobs;
use authorize::{Authorization, authorize, program_name};
#[cfg(test)]
use authorize::{command_is_allowlisted, load_allowed_commands};
pub(crate) use authorize::{command_names_any, command_tokens};
pub(crate) use capture::kill_process_group;
use capture::render_result;

pub(crate) const NAME: &str = "execute";

const EXECUTE_TOOL_CONFIG_FILE: &str = "openai-execute-tool.json";
pub(crate) const EXECUTE_TOOL_ENV_ALLOWLIST: &str = "AI_CHAT_EXECUTE_ALLOWLIST";
const EXECUTE_TOOL_CONFIG_OVERRIDE_ENV: &str = "DOGESH_EXECUTE_TOOL_CONFIG";
const CONFIG_DIR_PREFIX: &str = "dogesh";

/// Wall-clock budget for one agent-task `execute` when the caller does not ask
/// for one. Unchanged: a task is unattended, and a runaway command there has
/// nobody watching it.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// The same budget for an interactive `!`, where a person is watching and can
/// interrupt. The old shared value killed `cargo build` from a cold target
/// directory whenever the model did not think to ask for more.
const DEFAULT_INTERACTIVE_TIMEOUT_MS: u64 = 600_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
/// Ceiling for an agent task. Unchanged: `chat_status` and `chat_reset` do not
/// reach `runtime.jobs`, so a cron-driven run has nobody to notice a command
/// that asked for an hour.
const MAX_TASK_TIMEOUT_MS: u64 = 600_000;
/// Ceiling for an interactive `!`. Raised now that a long command no longer
/// blocks the shell: it becomes a job the user can see in `chat_status` and
/// stop with `chat_reset`, so this only has to bound a forgotten process.
const MAX_INTERACTIVE_TIMEOUT_MS: u64 = 3_600_000;
/// Per-stream budget applied before the global tool-output cap, so a chatty
/// stdout can never push stderr out of the result.
const MAX_STREAM_CHARS: usize = 3072;
/// Floor for that budget when the serialized result still does not fit.
const MIN_STREAM_CHARS: usize = 256;

/// How long this call waits before handing back a job handle.
///
/// The argument wins over the operator's setting, but is **clamped, not
/// trusted**: the schema advertises `maximum: 60000` and providers violate a
/// schema routinely, so an unclamped value would hold the shell for as long as
/// the command runs - which is now up to an hour.
fn resolve_yield_ms(requested: Option<u64>, configured: impl FnOnce() -> u64) -> u64 {
    match requested {
        Some(requested) => requested.min(crate::chatgpt::MAX_EXECUTE_YIELD_MS),
        None => configured(),
    }
}

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Run a shell command and return its exit code, stdout, and stderr. Pipes, redirections and `&&` are supported. A command the shell's safety policy considers risky asks the user first, so prefer one clear command over a long chain. Long output is truncated in the middle, so the end of a build or test log is preserved. If the command is still running when the wait expires, the result is a job handle (`job_id`, `status:\"running\"`) instead of an exit code - follow it with `job_status`/`job_output`, and never re-run the command to check on it.",
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
                    "yield_time_ms": {"type":"integer","minimum":0,"maximum":60000,"description":"How long to wait for the command before returning a job handle instead of a result."},
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "description": "Kill the command after this many milliseconds. Defaults to 120000 for an agent task and 600000 for an interactive `!` turn (AI_CHAT_EXECUTE_TIMEOUT_MS overrides the interactive default only)."
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

    // Before anything parses this line. Shell planning is side-effect-free and
    // substitution bodies stay deferred until evaluation (materialized in
    // `dsh::shell::materialize` and run via `dsh::shell::substitution` only
    // after gating and authorization), but the agent path deliberately keeps
    // refusing substitution constructs: handing an unchecked line to the
    // safety evaluation would otherwise run the inner pipeline before the
    // user is asked - and again when the command really runs. Pipes,
    // redirection and `&&` are the point of this tool; substitution is not,
    // and refusing it keeps the evaluation a judgement rather than an
    // execution.
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

    let is_task = proxy.agent_runtime().is_some();
    // A task keeps the old two-minute default and ten-minute ceiling: nobody is
    // watching it. An interactive turn gets longer ones because a person is,
    // and can stop the job.
    let (default_timeout_ms, max_timeout_ms) = if is_task {
        (DEFAULT_TIMEOUT_MS, MAX_TASK_TIMEOUT_MS)
    } else {
        (
            crate::chatgpt::resolve_execute_timeout_ms(proxy, DEFAULT_INTERACTIVE_TIMEOUT_MS),
            MAX_INTERACTIVE_TIMEOUT_MS,
        )
    };
    let timeout_ms = parsed
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(default_timeout_ms)
        .clamp(MIN_TIMEOUT_MS, max_timeout_ms);

    if let Some(runtime) = proxy.agent_runtime() {
        // Clone task inputs under a short lock, then release before touching
        // the proxy or spawning: a persistent task receives only the minimum
        // logical baseline plus explicit grants, never the full exported
        // environment.
        let (task_root, grant) = {
            let runtime = runtime.lock();
            (runtime.task.root.clone(), runtime.task.grant.clone())
        };
        let search_paths = proxy.command_search_paths();
        let snapshot = {
            let mut fetch = |key: &str| proxy.get_var(key);
            crate::agent::sandbox::SandboxRuntimeSnapshot::capture(
                search_paths,
                &mut fetch,
                &grant.environment,
            )
        };
        let (builder, config) = crate::agent::sandbox::command(
            command,
            cwd.as_deref().unwrap_or(&task_root),
            &grant,
            &crate::config_paths::agent_state_dir(),
            &snapshot,
        )
        .map_err(|e| e.to_string())?;
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
    // The explicit argument wins; otherwise the operator's setting decides how
    // long the shell is willing to wait before the command becomes a job.
    let yield_ms = resolve_yield_ms(parsed.get("yield_time_ms").and_then(|v| v.as_u64()), || {
        crate::chatgpt::resolve_execute_yield_ms(proxy)
    });

    jobs::run_as_job(
        command,
        cwd.as_deref(),
        Duration::from_millis(timeout_ms),
        yield_ms,
        proxy,
    )
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

#[cfg(test)]
pub(crate) mod tests;
