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

use crate::ShellProxy;
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
        authorize(command, &stages, proxy)?,
        Authorization::Cancelled
    ) {
        return Ok("Execution cancelled by user.".to_string());
    }

    let timeout_ms = parsed
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);

    let run_result = run_with_timeout(command, cwd.as_deref(), Duration::from_millis(timeout_ms))
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

    let note = match (run_result.timed_out, run_result.drain_incomplete) {
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

    None
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
    proxy: &mut dyn ChatToolHost,
) -> Result<Authorization, String> {
    // A skill script is arbitrary code that the agent can also write to, so it
    // is confirmed even when the rest of the policy would wave it through.
    let mut skill_script = false;
    for stage in stages {
        if is_skill_script_program(&stage.program, proxy)? {
            skill_script = true;
            break;
        }
    }

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
            format!("AI wants to run a skill script: `{command}`. \r\nProceed?")
        }
        // The `edit` and `str_replace` tools confirm every write; a write
        // spelled as a redirection is the same act and gets the same question.
        _ if redirects_a_write && !allowlisted => {
            format!("AI wants to run `{command}`, which writes to a file. \r\nProceed?")
        }
        AgentCommandVerdict::Allowed => return Ok(Authorization::Run),
        AgentCommandVerdict::Confirm(_) if allowlisted => return Ok(Authorization::Run),
        AgentCommandVerdict::Confirm(reason) => {
            format!("AI wants to run `{command}`. {reason} \r\nProceed?")
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
fn run_with_timeout(
    command: &str,
    cwd: Option<&Path>,
    timeout: Duration,
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
fn kill_process_group(child: &std::process::Child) {
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
pub(crate) mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use tempfile::tempdir;

    /// Serializes every test that touches `EXECUTE_TOOL_ENV_ALLOWLIST`: the env
    /// var is process-global and overrides the proxy's allowlist, so a test that
    /// sets it would otherwise decide what an unrelated concurrent test may run.
    pub(crate) static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    /// Take the env lock, ignoring poisoning.
    ///
    /// The guard protects a process-global environment variable, not an
    /// invariant, so a panicking test leaves nothing broken behind - but
    /// `unwrap()` on a poisoned mutex turned one real failure into a screenful
    /// of `PoisonError` that hid it.
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    use crate::test_support::TestShellProxy;
    type TestProxy = TestShellProxy;

    /// The shell variable is what `(vset ...)` and a plain `FOO=bar` write, so
    /// it has to be consulted before the process environment.
    /// The flag table only sees `sh -c '…'`. A shell reading stdin carries no
    /// flag, and `eval` is not an interpreter invocation at all - both left the
    /// guard classifying `printf` or `eval` while `sh` ran something else.
    #[test]
    fn code_the_guard_cannot_read_is_refused() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(hidden_code_source("eval", &args(&["rm -rf /"])).is_some());
        assert!(hidden_code_source("sh", &[]).is_some());
        // Built rather than written as a literal so the portability lint does
        // not read it as a claim about where the binary lives.
        assert!(hidden_code_source(&format!("{}/bash", "/bin"), &args(&["-s"])).is_some());

        // A script operand is a file the tools can read; not this rule's business.
        assert!(hidden_code_source("sh", &args(&["build.sh"])).is_none());
        assert!(hidden_code_source("python3", &args(&["main.py"])).is_none());
        // `sudo` is a wrapper; the stage for what it wraps is judged separately.
        assert!(hidden_code_source("sudo", &args(&["-u", "nobody"])).is_none());
        assert!(hidden_code_source("cargo", &args(&["test"])).is_none());
    }

    /// A backslash is literal inside single quotes, so treating it as an escape
    /// swallowed the closing quote and hid the redirection behind it.
    #[test]
    fn a_backslash_in_single_quotes_does_not_hide_a_redirection() {
        assert!(writes_by_redirection(r"echo 'a' > out"));
        assert!(writes_by_redirection("echo hi > out"));
        assert!(writes_by_redirection("echo hi >> out"));
        // Still not a file write.
        assert!(!writes_by_redirection("make 2>&1"));
        assert!(!writes_by_redirection("echo 'a > b'"));
    }

    #[test]
    fn load_allowlist_prefers_the_shell_variable_over_the_process_environment() {
        let _lock = env_lock();
        let _guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "from-process-env");
        assert_eq!(
            load_allowed_commands(vec![], Some("from-shell-var".to_string())).unwrap(),
            vec!["from-shell-var"]
        );
    }

    #[test]
    fn load_allowlist_prefers_env() {
        let _lock = env_lock();
        let _guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls,git\ncat");
        assert_eq!(
            load_allowed_commands(vec![], None).unwrap(),
            vec!["cat", "git", "ls"]
        );
    }

    #[test]
    fn load_allowlist_reads_config_file() {
        let _lock = env_lock();
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("allow.json");
        let contents = json!({ "allowed_commands": ["cargo"] }).to_string();
        std::fs::write(&config_path, contents).unwrap();

        let _env_guard = EnvGuard::set(
            EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
            config_path.to_str().unwrap(),
        );
        let _allow_env = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        assert_eq!(load_allowed_commands(vec![], None).unwrap(), vec!["cargo"]);
    }

    #[test]
    fn load_allowlist_merges_runtime_entries() {
        let _lock = env_lock();
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("allow.json");
        let contents = json!({ "allowed_commands": ["cargo"] }).to_string();
        std::fs::write(&config_path, contents).unwrap();

        let _env_guard = EnvGuard::set(
            EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
            config_path.to_str().unwrap(),
        );
        let _allow_env = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");

        let allowlist =
            load_allowed_commands(vec!["ls".to_string(), "cargo".to_string()], None).unwrap();
        assert_eq!(allowlist, vec!["cargo", "ls"]);
    }

    /// A command the policy denies is refused, and the refusal names it.
    #[test]
    fn run_reports_the_command_the_policy_denied() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Denied("not on this host".to_string()),
            confirm_result: true,
            ..TestProxy::default()
        };
        let result = run("{\"command\":\"cat README.md\"}", &mut proxy);
        let err = result.expect_err("command should be rejected");
        assert!(err.contains("`cat README.md`"), "{err}");
        assert!(err.contains("not on this host"), "{err}");
    }

    /// Not being on the allowlist is a question now, not a refusal.
    ///
    /// The allowlist ships empty, so the old "allowlisted or rejected" rule
    /// meant a fresh install could not run a single command.
    #[test]
    fn run_asks_rather_than_refusing_a_command_outside_the_allowlist() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            execute_allowlist: vec![],
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Confirm("unknown command".to_string()),
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let result = run("{\"command\":\"cat README.md\"}", &mut proxy).unwrap();

        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(result.contains("cancelled by user"), "{result}");
    }

    /// "Always" is what keeps a long run from becoming a prompt per step.
    #[test]
    fn run_remembers_an_always_approval_for_the_session() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        let mut proxy = TestProxy {
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Confirm("unknown command".to_string()),
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..TestProxy::default()
        };

        run("{\"command\":\"true\"}", &mut proxy).unwrap();

        assert_eq!(proxy.agent_session_allowlist, vec!["true".to_string()]);
    }

    /// Pipelines run now. Refusing them outright meant `cargo test | tail`
    /// could not be expressed, so every multi-step job cost a round trip per
    /// step; what makes it safe is the policy verdict, not a token blacklist.
    #[test]
    fn run_executes_a_pipeline_the_policy_allows() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        let mut proxy = TestProxy {
            execute_allowlist: vec![],
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Allowed,
            confirm_result: false,
            ..TestProxy::default()
        };

        let result = run("{\"command\":\"printf 'a\\nb\\n' | tail -1\"}", &mut proxy).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["exit_code"], 0);
        assert_eq!(parsed["stdout"].as_str().unwrap().trim(), "b");
    }

    /// The shell's parser *runs* `$(...)` while building its job list, so a
    /// line carrying one would have executed during the safety check - before
    /// the user was asked, and again for real afterwards.
    #[test]
    fn substitution_is_refused_before_anything_parses_the_line() {
        let _lock = env_lock();
        let mut proxy = TestProxy {
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Allowed,
            ..TestProxy::default()
        };

        for command in [
            "echo $(whoami)",
            "echo `whoami`",
            "diff <(ls) <(ls)",
            "(cd /tmp && ls)",
        ] {
            let arguments = json!({ "command": command }).to_string();
            let err = run(&arguments, &mut proxy).expect_err("{command} must not reach the parser");
            assert!(err.contains("does not allow"), "{command}: {err}");
        }
    }

    /// Single quotes suppress every expansion, so a `$(` inside them is text.
    #[test]
    fn a_dollar_paren_inside_single_quotes_is_not_a_substitution() {
        assert!(substitution_construct("grep '$(' file").is_none());
        assert!(substitution_construct("echo $(date)").is_some());
    }

    /// Double quotes do *not* suppress `$(...)`, and an apostrophe inside them
    /// is a literal. Treating that apostrophe as an opening quote made the rest
    /// of the line look quoted, so this substitution reached the parser - which
    /// runs it.
    #[test]
    fn quoting_rules_match_the_shells() {
        let detected = |command: &str| substitution_construct(command).is_some();

        // Still a substitution despite the quotes around it.
        assert!(detected(r#"echo "it's $(whoami)""#));
        assert!(detected(r#"echo "$(date)""#));
        assert!(detected(r#"echo "it's `date`""#));

        // Genuinely literal.
        assert!(!detected(r#"grep "it's fine" file"#));
        assert!(!detected("echo 'no $(sub) here'"));
        assert!(!detected(r#"echo "a (b) c""#));
        assert!(!detected("printf 'a\nb\n' | tail -1"));

        // `echo \$(date)` escapes the dollar but leaves a bare `(`, which is a
        // subshell. Refusing it is the safe reading: this check runs before the
        // parser, where a false positive costs one rephrasing and a false
        // negative costs an unreviewed execution.
        assert!(detected("echo \\$(date)"));
    }

    /// A newline is ordinary whitespace to the tokenizer, so a second command
    /// hid behind the first: only `ls` was inspected, and a bare `ls` entry
    /// then waved the whole line through without a prompt.
    #[test]
    fn a_newline_starts_a_new_stage() {
        let stages = command_stages("ls\nbash -c 'rm -rf ~'").unwrap();
        let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();

        assert_eq!(seen, vec!["ls", "bash"]);
    }

    /// A newline inside quotes is data. Splitting on it left two fragments
    /// with unbalanced quotes and failed to parse a legitimate `printf`.
    #[test]
    fn a_quoted_newline_does_not_start_a_new_stage() {
        let stages = command_stages("printf 'a\nb\n' | tail -1").unwrap();
        let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();

        assert_eq!(seen, vec!["printf", "tail"]);
    }

    #[test]
    fn a_hidden_second_command_still_reaches_the_string_eval_refusal() {
        let _lock = env_lock();
        let mut proxy = TestProxy {
            execute_allowlist: vec!["ls".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Allowed,
            ..TestProxy::default()
        };

        let arguments = json!({ "command": "ls\nbash -c 'echo hi'" }).to_string();
        let err = run(&arguments, &mut proxy).expect_err("the bash -c must be seen");

        assert!(err.contains("hands it a string to execute"), "{err}");
    }

    /// `SafetyGuard` classifies by program name, so `sudo` used to hide the
    /// `rm` behind it from every `rm` rule.
    #[test]
    fn a_wrapper_does_not_hide_the_command_it_runs() {
        let stages = command_stages("sudo rm -rf /tmp/x").unwrap();
        let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();

        assert!(seen.contains(&"rm"), "{seen:?}");
    }

    /// A redirection is a file write, and the agent's file writes are confirmed.
    #[test]
    fn a_redirected_write_is_recognised_but_a_descriptor_merge_is_not() {
        assert!(writes_by_redirection("echo x > ~/.ssh/authorized_keys"));
        assert!(writes_by_redirection("cargo build >> build.log"));
        assert!(!writes_by_redirection("cargo test 2>&1 | tail -5"));
        assert!(!writes_by_redirection("echo 'a > b'"));
    }

    #[test]
    fn a_redirected_write_asks_even_when_the_policy_allows_the_command() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Allowed,
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let arguments = json!({ "command": "echo hi > /tmp/dsh-write-check" }).to_string();
        let result = run(&arguments, &mut proxy).unwrap();

        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(result.contains("cancelled by user"), "{result}");
    }

    /// Approving one line must not approve a longer one that starts with it.
    #[test]
    fn a_session_approval_does_not_widen_by_prefix() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
        let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Confirm("risky".to_string()),
            agent_session_allowlist: vec!["true one".to_string()],
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        // The approved line runs without asking again.
        run(&json!({ "command": "true one" }).to_string(), &mut proxy).unwrap();
        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        // One extra argument is a different command, and is asked about.
        let result = run(
            &json!({ "command": "true one two" }).to_string(),
            &mut proxy,
        )
        .unwrap();
        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(result.contains("cancelled by user"), "{result}");
    }

    /// Every stage of a pipeline is inspected, not just the first.
    #[test]
    fn command_stages_split_on_operators_and_skip_assignments() {
        let stages = command_stages("FOO=1 cargo test 2>&1 | tail -5 && echo done").unwrap();
        let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();
        assert_eq!(seen, vec!["cargo", "tail", "echo"]);
        assert_eq!(stages[0].args, vec!["test".to_string(), "2>&1".to_string()]);
    }

    #[test]
    fn run_rejects_string_eval_flags() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "bash");
        let mut proxy = TestProxy {
            execute_allowlist: vec!["bash".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            confirm_result: true,
            ..TestProxy::default()
        };

        // A combined cluster is the same flag wearing a hat.
        for command in ["bash -lc 'echo hi'", "bash -ic 'echo hi'"] {
            let arguments = format!("{{\"command\":\"{command}\"}}");
            let result = run(&arguments, &mut proxy);
            assert!(result.is_err(), "{command} was allowed through");
            assert!(
                result.unwrap_err().contains("hands it a string to execute"),
                "unexpected refusal for {command}"
            );
        }
    }

    /// Every spelling of "here is some code" has to be caught, and nothing else.
    #[test]
    fn string_eval_flags_are_recognised_in_every_spelling() {
        let eval = [
            ("bash", vec!["-c", "echo hi"]),
            ("bash", vec!["-lc", "echo hi"]),
            ("bash", vec!["-ic", "echo hi"]),
            ("sh", vec!["-euc", "echo hi"]),
            ("bash", vec!["-o", "pipefail", "-c", "echo hi"]),
            ("zsh", vec!["-ic", "echo hi"]),
            ("fish", vec!["--command", "echo hi"]),
            ("fish", vec!["-C", "echo hi"]),
            ("python3", vec!["-Ec", "print(1)"]),
            ("python3", vec!["-c", "print(1)"]),
            ("perl", vec!["-E", "say 1"]),
            ("perl", vec!["-lne", "print"]),
            ("ruby", vec!["-e", "puts 1"]),
            ("node", vec!["--eval", "1"]),
            ("node", vec!["-pe", "1"]),
            ("pwsh", vec!["-EncodedCommand", "AAA"]),
            ("powershell", vec!["-Comm", "dir"]),
        ];
        for (program, args) in eval {
            let args: Vec<String> = args.into_iter().map(str::to_string).collect();
            assert!(
                string_eval_flag(program, &args).is_some(),
                "{program} {args:?} should have been recognised"
            );
        }

        let plain = [
            // A script is not a string, even when its own arguments look like
            // eval flags.
            ("bash", vec!["script.sh"]),
            ("bash", vec!["script.sh", "-c"]),
            ("bash", vec!["--", "-c"]),
            // Options whose value merely contains an eval letter.
            ("python3", vec!["-Wonce", "script.py"]),
            ("python3", vec!["-m", "http.server"]),
            ("perl", vec!["-Mencoding", "script.pl"]),
            ("ruby", vec!["-E", "utf-8", "script.rb"]),
            ("node", vec!["server.js"]),
            ("node", vec!["-r", "esm", "server.js"]),
            // Not an interpreter at all.
            ("git", vec!["-c", "user.name=x", "status"]),
            ("ls", vec!["-c"]),
        ];
        for (program, args) in plain {
            let args: Vec<String> = args.into_iter().map(str::to_string).collect();
            assert_eq!(
                string_eval_flag(program, &args),
                None,
                "{program} {args:?} should have been left alone"
            );
        }
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

    /// A `git` entry must not silently authorise `/tmp/git`.
    ///
    /// It is no longer a flat refusal - the policy decides - but it must still
    /// not take the allowlist fast path, so the user is asked.
    #[test]
    fn run_does_not_let_a_path_qualified_program_ride_a_basename_allowlist() {
        let _lock = env_lock();
        let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "git");
        let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            execute_allowlist: vec!["git".to_string()],
            current_dir: std::env::current_dir().unwrap(),
            agent_verdict: AgentCommandVerdict::Confirm("unknown command".to_string()),
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let result = run("{\"command\":\"/tmp/git status\"}", &mut proxy).unwrap();

        assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(result.contains("cancelled by user"), "{result}");
    }

    #[test]
    fn run_skips_confirmation_for_allowlisted_command() {
        let _lock = env_lock();
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
        let _lock = env_lock();
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
        let _lock = env_lock();
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
        let _lock = env_lock();
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
        let _lock = env_lock();

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
        // Returning is not enough: the script's own output has to survive the
        // grace period. Waiting for a single end-of-stream message meant giving
        // up with an empty stdout even though the line had already been read.
        assert!(
            parsed["stdout"].as_str().unwrap().contains("started"),
            "the script's output was dropped: {result}"
        );
    }

    /// The end of a long output is where the compiler error is, so a capture
    /// that overflows its cap has to keep the tail, not just the head.
    #[test]
    fn a_capture_over_the_cap_keeps_the_tail() {
        let mut capture = CappedCapture::default();
        capture.push(b"HEAD-MARKER");
        capture.push(&vec![b'x'; MAX_CAPTURED_BYTES * 2]);
        capture.push(b"TAIL-MARKER");

        let snapshot = String::from_utf8_lossy(&capture.snapshot()).into_owned();
        assert!(snapshot.starts_with("HEAD-MARKER"), "lost the head");
        assert!(snapshot.ends_with("TAIL-MARKER"), "lost the tail");
        assert!(
            snapshot.contains("dropped"),
            "the omission is not reported: {}",
            &snapshot[..80.min(snapshot.len())]
        );
    }

    /// Under the cap nothing is rewritten, marker included.
    #[test]
    fn a_capture_under_the_cap_is_verbatim() {
        let mut capture = CappedCapture::default();
        capture.push(b"one\n");
        capture.push(b"two\n");

        assert_eq!(capture.snapshot(), b"one\ntwo\n");
    }

    #[test]
    fn output_written_before_a_grandchild_outlives_the_command_is_kept() {
        // Same shape as above with more to lose: every line the script itself
        // printed must come back, not just the first.
        let _lock = env_lock();

        let dir = tempdir().unwrap();
        let script = dir.path().join("chatty-spawner.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep 5 &\ni=0\nwhile [ $i -lt 200 ]; do echo \"line-$i\"; i=$((i+1)); done\n",
        )
        .unwrap();
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

        let result = run(&format!("{{\"command\":\"{allowlist}\"}}"), &mut proxy).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let stdout = parsed["stdout"].as_str().unwrap();

        assert!(stdout.contains("line-0"), "lost the head: {result}");
        assert!(stdout.contains("line-199"), "lost the tail: {result}");
    }

    #[test]
    fn a_large_stdout_does_not_evict_stderr() {
        // Regression: the whole result JSON used to be cut from the front, so a
        // chatty stdout dropped the error message the model needed.
        let _lock = env_lock();
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

    pub(crate) struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        pub(crate) fn set(key: &'static str, value: &str) -> Self {
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
