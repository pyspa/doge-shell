use serde::Deserialize;
use serde_json::{Value, json};
use shell_words::split;
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use xdg::BaseDirectories;

use crate::shell_capabilities::{AgentCommandVerdict, ApprovalDecision, ChatToolHost};
use anyhow::Result;
use dsh_types::safety_policy::{string_eval_flag, substitution_construct};

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

/// What the policy and the user together decided about a command.
enum Authorization {
    Run,
    Cancelled,
}

/// Decide whether the command may run, asking the user when the policy says so.
///
/// The old rule was "in the allowlist or refused", and the allowlist starts
/// empty, so out of the box the agent could not run a single command. Now the
/// allowlist is the fast path and everything else is judged by the shell's own
/// `SafetyGuard` at the configured safety level: harmless commands run and
/// risky ones ask.
///
/// The allowlist really is a *skip*, not an extra check: an operator who wrote
/// `(chat-execute-add "rm")` has said they do not want to be asked about `rm`,
/// and the guard's warning is suppressed for it. That is what the entry means,
/// so it is the entry - not this function - that decides how much to trust.
fn authorize(
    command: &str,
    stages: &[CommandStage],
    execution_dir: Option<&Path>,
    proxy: &mut dyn ChatToolHost,
) -> Result<Authorization, String> {
    // A skill script is arbitrary code that the agent can also write to, so it
    // is confirmed even when the rest of the policy would wave it through -
    // including under `agent run`, where `--allow-command` does not cover it.
    let skill_script = touches_skill_file(stages, execution_dir, proxy)?;

    // The shell's runtime list plus the JSON config and the environment
    // variable; dropping this merge would have quietly disabled
    // `~/.config/dsh/openai-execute-tool.json`.
    let allowlist = load_allowed_commands(
        proxy.agent_allowlist(),
        proxy.get_var(EXECUTE_TOOL_ENV_ALLOWLIST),
    )?;

    // Configured entries match by token prefix, because a person wrote
    // `cargo test` meaning "and its arguments". A session "always" answer is
    // matched against the exact line the user was shown instead: approving
    // `rm -rf target` must not go on to approve `rm -rf target ~/documents`.
    if proxy.agent_runtime().is_some() {
        // A skill script is not covered by `--allow-command`. The grant names a
        // command line the person read; a skill script is a file the agent can
        // also write, and one that arrives with a `git clone`. Leaving this to
        // `evaluate_agent_command` was how the rule below - "confirmed even
        // when the rest of the policy would wave it through" - stopped being
        // true the moment the same line ran under `agent run`.
        if skill_script {
            proxy
                .request_agent_approval(&format!(
                    "{command}: running a skill script needs its own approval"
                ))
                .map_err(|e| e.to_string())?;
            return Err(format!(
                "agent: skill script permission required: {command}"
            ));
        }
        return match proxy.evaluate_agent_command(command) {
            AgentCommandVerdict::Allowed => Ok(Authorization::Run),
            AgentCommandVerdict::Denied(reason) => Err(reason),
            AgentCommandVerdict::Confirm(reason) => {
                proxy
                    .request_agent_approval(&format!("{command}: {reason}"))
                    .map_err(|e| e.to_string())?;
                Err(format!("agent: command permission required: {command}"))
            }
        };
    }
    let approved_exactly = proxy
        .agent_session_approvals()
        .iter()
        .any(|approved| approved == command);
    let allowlisted = approved_exactly
        || stages
            .iter()
            .all(|stage| command_is_allowlisted(&stage.program, &stage.args, &allowlist));

    let verdict = proxy.evaluate_agent_command(command);
    let redirects_a_write = writes_by_redirection(command);

    let prompt = match verdict {
        AgentCommandVerdict::Denied(reason) => {
            return Err(format!("chat: execute tool refused `{command}`: {reason}"));
        }
        _ if skill_script => {
            format!("AI wants to run a skill script: `{command}`")
        }
        // The `edit` and `str_replace` tools confirm every write; a write
        // spelled as a redirection is the same act and gets the same question.
        _ if redirects_a_write && !allowlisted => {
            format!("AI wants to run `{command}`, which writes to a file")
        }
        AgentCommandVerdict::Allowed => return Ok(Authorization::Run),
        AgentCommandVerdict::Confirm(_) if allowlisted => return Ok(Authorization::Run),
        AgentCommandVerdict::Confirm(reason) => {
            format!("AI wants to run `{command}`. {reason}")
        }
    };

    match proxy
        .request_agent_approval(&prompt)
        .map_err(|err| format!("chat: confirmation failed: {err}"))?
    {
        ApprovalDecision::Allow => Ok(Authorization::Run),
        ApprovalDecision::AllowAlways => {
            proxy.remember_agent_approval(command);
            Ok(Authorization::Run)
        }
        ApprovalDecision::Deny => Ok(Authorization::Cancelled),
    }
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
    cancelled: bool,
    /// The readers never reached end of stream, so what follows is whatever had
    /// arrived when the grace period ran out.
    drain_incomplete: bool,
}

/// Drain a child pipe on its own thread, into a buffer the caller can read at
/// any time.
///
/// Polling only for exit deadlocks as soon as the child fills a pipe buffer, so
/// both streams have to be read while we wait. The buffer is shared rather than
/// sent once at EOF: a surviving grandchild holds the write end open, so EOF may
/// never arrive, and waiting for a single end-of-stream message meant giving up
/// with *nothing* — a command whose output had already been read in full still
/// reported an empty stdout.
///
/// The pipe keeps being drained past `MAX_CAPTURED_BYTES`, so a chatty child
/// never blocks on a full pipe while memory stays bounded.
fn drain_pipe<R>(pipe: Option<R>) -> DrainedPipe
where
    R: Read + Send + 'static,
{
    let drained = DrainedPipe {
        buffer: Arc::new(Mutex::new(CappedCapture::default())),
        at_eof: Arc::new(AtomicBool::new(false)),
    };
    let writer = Arc::clone(&drained.buffer);
    let at_eof = Arc::clone(&drained.at_eof);

    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0_u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        // A panic elsewhere must not cost us the output we
                        // already have: the buffer is a plain byte log, so a
                        // poisoned lock has nothing broken to protect.
                        let mut buffer = writer
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        buffer.push(&chunk[..read]);
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        at_eof.store(true, Ordering::Release);
    });

    drained
}

/// A bounded byte log that keeps both ends of what it was given.
///
/// Compiler errors, test failures and stack traces live at the *end* of a
/// command's output — the same reason `truncate_middle` cuts the middle — so a
/// cap that keeps the first N bytes and throws the rest away hides the very
/// thing the model has to react to.
#[derive(Default)]
struct CappedCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: usize,
}

impl CappedCapture {
    const HEAD_BYTES: usize = MAX_CAPTURED_BYTES / 2;
    const TAIL_BYTES: usize = MAX_CAPTURED_BYTES - Self::HEAD_BYTES;

    fn push(&mut self, mut bytes: &[u8]) {
        let head_room = Self::HEAD_BYTES.saturating_sub(self.head.len());
        if head_room > 0 {
            let take = head_room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }

        self.tail.extend(bytes);
        while self.tail.len() > Self::TAIL_BYTES {
            self.tail.pop_front();
            self.dropped += 1;
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        let mut out = self.head.clone();
        if self.dropped > 0 {
            out.extend_from_slice(
                format!(
                    "\n... (dropped {} bytes from the middle of the capture) ...\n",
                    self.dropped
                )
                .as_bytes(),
            );
        }
        out.extend(self.tail.iter().copied());
        out
    }
}

/// A pipe being drained in the background: what has been read so far, and
/// whether the reader reached the end of the stream.
struct DrainedPipe {
    buffer: Arc<Mutex<CappedCapture>>,
    at_eof: Arc<AtomicBool>,
}

impl DrainedPipe {
    fn at_eof(&self) -> bool {
        self.at_eof.load(Ordering::Acquire)
    }

    /// Whatever the drain thread has collected so far.
    ///
    /// Called once the child is gone (or the deadline passed): the reader may
    /// still be blocked on a grandchild's copy of the write end, and its
    /// progress is worth more than the EOF that is never coming.
    fn snapshot(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot()
    }
}

/// Run `command` under `sh -c`.
///
/// Direct `Command::new` execution meant no pipes, no redirection, no `&&` and
/// no globbing, so `cargo test 2>&1 | tail -40` could not be expressed at all
/// and every multi-step job cost one round trip per step. The shell here is
/// what makes the command line real; what makes it safe is `authorize`, which
/// has already put the whole line through the shell's own parser and safety
/// guard.
fn run_with_timeout_cancel(
    command: &str,
    cwd: Option<&Path>,
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
) -> Result<CapturedRun, String> {
    let mut builder = Command::new("sh");
    builder
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout can take the whole tree down instead
        // of orphaning whatever the command spawned.
        .process_group(0);

    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }

    let mut child = builder.spawn().map_err(|err| err.to_string())?;

    let stdout_reader = drain_pipe(child.stdout.take());
    let stderr_reader = drain_pipe(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => return Err(err.to_string()),
        }

        cancelled = cancel();
        if cancelled || Instant::now() >= deadline {
            kill_process_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            timed_out = !cancelled;
            break None;
        }

        std::thread::sleep(TIMEOUT_POLL_INTERVAL);
    };

    // Bounded: a surviving grandchild still holds the write end of the pipe, so
    // EOF may never arrive. Give the readers a moment to catch up with what the
    // child already wrote, then take whatever they have.
    let drained = wait_for_drain(&[&stdout_reader, &stderr_reader], DRAIN_GRACE);
    let stdout = stdout_reader.snapshot();
    let stderr = stderr_reader.snapshot();

    Ok(CapturedRun {
        status,
        stdout,
        stderr,
        timed_out,
        cancelled,
        drain_incomplete: !drained,
    })
}

/// Wait for the readers to reach end of stream, or for `grace` to run out.
///
/// A normal command hits EOF within microseconds of exiting; only a surviving
/// grandchild holding the write end open runs the clock down, and that is
/// exactly the case the grace period bounds.
///
/// Returns whether every reader got there, so the caller can say so when the
/// output it hands back is only as much as had arrived.
fn wait_for_drain(readers: &[&DrainedPipe], grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if readers.iter().all(|reader| reader.at_eof()) {
            return true;
        }
        std::thread::sleep(DRAIN_POLL_INTERVAL);
    }
    readers.iter().all(|reader| reader.at_eof())
}

/// Signal the whole group the child leads, so background grandchildren die too.
pub(crate) fn kill_process_group(child: &std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // `process_group(0)` made the child its own group leader, so pgid == pid.
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
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
        return true;
    }

    // Prefix, not equality. `cargo test` used to authorise exactly
    // `cargo test` and nothing else, so `cargo test -p dsh-builtin` was
    // refused - while a bare `cargo` entry authorised `cargo publish`. Neither
    // extreme is what an allowlist is for.
    let expected = &entry_tokens[1..];
    args.len() >= expected.len() && args[..expected.len()] == *expected
}

fn command_is_allowlisted(program: &str, args: &[String], allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|entry| allowlist_entry_matches(entry, program, args))
}

/// Does any stage of `command` run a program named by one of `entries`?
///
/// `entries` uses the same word-prefix form as `AI_CHAT_EXECUTE_ALLOWLIST`, so
/// `"git push"` covers `git push --force` while `"rm"` covers every `rm`. Every
/// stage is judged and wrappers are looked through (`command_candidates`), so
/// `sudo rm -rf x`, `timeout 5 rm x` and `echo hi | rm -rf x` all answer `rm`.
///
/// # The polarity here is the opposite of the allowlist's
///
/// For the allowlist a match means "run without asking", so a matcher that
/// grows stricter refuses more - the safe direction. For a hook's `match` a
/// match means "run this check", so the same matcher growing stricter runs the
/// check *less*: a gate weakening in silence. Anything that changes
/// `allowlist_entry_matches` has to be read with both callers in mind, which is
/// what `command_names_any_looks_through_wrappers_and_stages` pins down.
///
/// An unparseable command line answers **`true`**. A hook's matcher is all that
/// stands between a command and a check the user asked for, and a mismatched
/// quote must not be a way to skip it. `authorize` refuses such a line
/// separately, so firing costs nothing but one hook run.
pub(crate) fn command_names_any(command: &str, entries: &[String]) -> bool {
    let Some(stages) = readable_stages(command) else {
        return true;
    };
    stages
        .iter()
        .any(|stage| command_is_allowlisted(&stage.program, &stage.args, entries))
}

/// Every token of every stage, or `None` when the line cannot be read.
///
/// Same reasoning as `touches_skill_file`: the program alone misses
/// `bash <path>/run.sh`, because `bash` is not a transparent wrapper. A caller
/// asking "does this command line mention such a path" has to see the arguments
/// too. `None` is kept distinct from an empty vector so the caller decides what
/// an unreadable line means rather than inheriting "mentions nothing".
pub(crate) fn command_tokens(command: &str) -> Option<Vec<String>> {
    Some(
        readable_stages(command)?
            .into_iter()
            .flat_map(|stage| std::iter::once(stage.program).chain(stage.args))
            .collect(),
    )
}

fn readable_stages(command: &str) -> Option<Vec<CommandStage>> {
    command_stages(command).ok()
}

/// Does any part of this command line reach a file that ships with a skill?
///
/// Every skill root counts, the project one included. A skill arrives with a
/// `git clone` and the prompt actively points the model at it, so a file under
/// `<project>/.dsh/skills` is exactly the case that must not fall through to
/// the ordinary command policy and run unasked under `loose`.
///
/// Judged over **every token of every stage**, not just the program. Only the
/// program was checked at first, and `bash <skill>/run.sh` walked straight
/// past: `bash` is not a transparent wrapper (`COMMAND_WRAPPERS` is right not
/// to list it - `bash foo.sh` runs a script, it does not pass through), so the
/// stage stayed `("bash", ["…run.sh"])` and the program alone said "no".
///
/// This deliberately also asks about reading one - `cat <skill>/SKILL.md` gets
/// a prompt. Telling execution from reading by looking at the arguments means
/// guessing what the program does with them, and guessing wrong in the
/// permissive direction is how the hole above happened. An extra question about
/// a file the agent can also write is the cheaper mistake.
///
/// `AI_CHAT_PROJECT_SKILLS=0` hides project skills from the prompt; it does not
/// make running one of their scripts safe, so this always considers both roots.
fn touches_skill_file(
    stages: &[CommandStage],
    execution_dir: Option<&Path>,
    proxy: &mut dyn ChatToolHost,
) -> Result<bool, String> {
    let shell_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    // The directory the command will actually run in. `execute` takes a `cwd`
    // argument, and resolving relative tokens against the shell's directory
    // instead let `{"command": "./run.sh", "cwd": "<skill dir>"}` past.
    let base = execution_dir.unwrap_or(&shell_dir);

    let roots: Vec<PathBuf> = crate::chatgpt::skills::skill_roots(Some(base), true)
        .iter()
        .map(|root| {
            std::fs::canonicalize(&root.path).unwrap_or_else(|_| super::normalize_path(&root.path))
        })
        .collect();
    if roots.is_empty() {
        return Ok(false);
    }

    for stage in stages {
        if std::iter::once(&stage.program)
            .chain(stage.args.iter())
            .any(|token| token_is_within(token, base, &roots))
        {
            return Ok(true);
        }
    }

    Ok(false)
}

fn token_is_within(token: &str, base: &Path, roots: &[PathBuf]) -> bool {
    // A bare word is a PATH lookup, not a path into a skill.
    if !token.contains('/') && !Path::new(token).is_absolute() {
        return false;
    }
    // Options like `--config=x` are not paths; a real path token starting with
    // `-` would have to be written `./-foo` anyway.
    if token.starts_with('-') {
        return false;
    }

    // Resolved here rather than through `resolve_tool_path`, which is an access
    // decision: under a task it refuses any path outside the grants, so every
    // ungranted skill script came back as "not a skill script" and fell through
    // to the ordinary command policy - the exact opposite of what this is for.
    let Ok(expanded) = shellexpand::full(token) else {
        return false;
    };
    let path = Path::new(expanded.as_ref());
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let Ok(resolved) = super::resolve_with_existing_ancestor(&absolute) else {
        return false;
    };

    roots.iter().any(|root| resolved.starts_with(root))
}

/// Every source of "the agent may run this without asking", merged.
///
/// The environment variable used to return early and win outright, so setting
/// one entry silently discarded `config.lisp` and the JSON config - a trap,
/// because nothing said the other two had stopped applying.
fn load_allowed_commands(
    runtime_allowed: Vec<String>,
    shell_value: Option<String>,
) -> Result<Vec<String>, String> {
    let mut allowlist = runtime_allowed;

    // Shell variable first, process environment second - the order every other
    // AI setting resolves in. Reading only `std::env` made this the one key
    // that `config.lisp` could not set without an `export`.
    if let Some(mut from_env) = read_allowlist_from_env(shell_value) {
        allowlist.append(&mut from_env);
    }

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

fn read_allowlist_from_env(shell_value: Option<String>) -> Option<Vec<String>> {
    let raw = shell_value.or_else(|| env::var(EXECUTE_TOOL_ENV_ALLOWLIST).ok())?;
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
pub(crate) mod tests;
