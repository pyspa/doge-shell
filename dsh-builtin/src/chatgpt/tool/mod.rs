use parking_lot::RwLock;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::hooks::{self, HookContext};
use super::mcp::McpManager;
use crate::ShellProxy;
use crate::agent::ToolOutcome;
use crate::safety_policy::{self, SafetyLevel};
use crate::shell_capabilities::{AgentCommandVerdict, ApprovalDecision, ChatToolHost};

mod edit;
pub(crate) mod execute;
mod gitignore;
mod ls;
mod read;
mod replace;
mod search;
mod shell_context;
mod shell_history;
pub(crate) mod skill;

/// Global backstop for the size of a single tool result. Individual tools apply
/// their own tighter limits first so that the important part of their output
/// survives this cut.
///
/// Shared with the shell-side loop, which used half this and so fed the same
/// tool a different amount of room depending on the entry point.
pub(crate) const MAX_OUTPUT_LENGTH: usize = dsh_openai::turn::limits::MAX_TOOL_OUTPUT_CHARS;

#[derive(Debug)]
pub struct ToolExecution {
    pub content: String,
    pub outcome: ToolOutcome,
}

#[derive(Debug)]
pub struct ToolCallError {
    message: String,
    pub outcome: ToolOutcome,
}

impl From<String> for ToolCallError {
    fn from(message: String) -> Self {
        Self {
            message,
            outcome: ToolOutcome::Failure,
        }
    }
}

impl From<&str> for ToolCallError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl From<super::mcp::McpCallError> for ToolCallError {
    fn from(error: super::mcp::McpCallError) -> Self {
        let outcome = if error.outcome_unknown() {
            ToolOutcome::OutcomeUnknown
        } else {
            ToolOutcome::Failure
        };
        Self {
            message: error.to_string(),
            outcome,
        }
    }
}

impl std::fmt::Display for ToolCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub fn build_tools() -> Vec<Value> {
    vec![
        edit::definition(),
        execute::definition(),
        ls::definition(),
        read::definition(),
        replace::definition(),
        search::definition(),
        shell_context::definition(),
        shell_history::definition(),
        skill::definition(),
    ]
}

pub fn execute_tool_call(
    tool_call: &Value,
    mcp: &Arc<RwLock<McpManager>>,
    hooks: &HookContext,
    proxy: &mut dyn ChatToolHost,
) -> Result<ToolExecution, ToolCallError> {
    let function = tool_call
        .get("function")
        .ok_or_else(|| "chat: tool call missing function".to_string())?;

    let name = function
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: tool call missing function name".to_string())?;

    let arguments = function
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    // Log tool execution
    let logged_arguments = redact_tool_arguments(arguments);
    eprintln!(
        "\x1b[36m🔧 [Tool] {} ({})\x1b[0m",
        name,
        truncate_args(&logged_arguments)
    );

    // Take the lock for each short call rather than holding it across the
    // confirmation prompt: the user can sit on that question for a long time,
    // and `mcp connect` in another turn needs the write lock.
    let is_mcp_tool = mcp.read().has_tool_binding(name);

    // One seam for builtin, MCP and task tools alike. Putting this in each tool
    // instead would mean the next tool someone adds quietly has no hooks.
    //
    // Hooks run before the safety policy: a hook that refuses saves the user a
    // question, and a hook that says nothing changes nothing about what the
    // guard does next.
    let tool_call_id = tool_call.get("id").and_then(Value::as_str);
    let kind = tool_kind(name, is_mcp_tool);
    let pre = hooks.fire(
        hooks::HookEvent::PreToolUse,
        hooks::HookSubject::tool(name, arguments),
        || hooks::tool_detail(name, tool_call_id, kind, arguments),
        &|| proxy.is_canceled(),
    );
    if let Some((hook, reason)) = pre.denied() {
        return Ok(ToolExecution {
            content: format!("Blocked by hook `{hook}`: {reason}"),
            outcome: ToolOutcome::Failure,
        });
    }
    if let Some((hook, reason)) = pre.asked()
        && !confirm_agent_action(
            proxy,
            &hooks::approval_key(hook, name),
            &format!("hook `{hook}` flagged `{name}`: {reason}"),
        )?
    {
        return Ok(ToolExecution {
            content: format!("Blocked by hook `{hook}`: {reason}"),
            outcome: ToolOutcome::Failure,
        });
    }

    // After the hook and after any approval: this is how long the tool took,
    // not how long someone took to answer a question about it.
    let started = std::time::Instant::now();
    let dispatched = dispatch_tool(name, arguments, is_mcp_tool, mcp, proxy);
    let elapsed = started.elapsed();

    let failed = dispatched.is_err();
    let raw = match &dispatched {
        Ok(result) => result.clone(),
        Err(err) => err.message.clone(),
    };

    // Schemas must reach the host unchanged when registering discovered tools.
    let mut content = if name == "tool_search" && !failed {
        raw
    } else {
        truncate_output(raw)
    };
    let mut outcome = match &dispatched {
        Ok(_) => {
            if result_failed(&content) {
                ToolOutcome::Failure
            } else {
                ToolOutcome::Success
            }
        }
        Err(err) => err.outcome,
    };

    // A hook may add text to what the model reads before the result, so a
    // policy reminder arrives with the thing it is about.
    if let Some(note) = pre.context_note() {
        content = format!("{note}\n\n{content}");
    }

    // Fired for a failed call too. An audit hook that pairs pre with post was
    // otherwise left with an unmatched open event for exactly the calls it most
    // wants to see.
    let post = hooks.fire(
        hooks::HookEvent::PostToolUse,
        hooks::HookSubject::tool(name, arguments),
        || {
            post_tool_detail(
                name,
                tool_call_id,
                kind,
                arguments,
                &content,
                outcome,
                elapsed,
            )
        },
        &|| proxy.is_canceled(),
    );
    // The tool has already run: a `deny` here cannot undo it, but it can stop
    // the model from reading the result as a success.
    if let Some((hook, reason)) = post.denied() {
        content = format!("Rejected by hook `{hook}` after the tool ran: {reason}\n{content}");
        outcome = ToolOutcome::Failure;
    }
    if let Some(note) = post.context_note() {
        content.push_str("\n\n");
        content.push_str(&note);
    }

    if failed {
        return Err(ToolCallError {
            message: content,
            outcome,
        });
    }

    Ok(ToolExecution { content, outcome })
}

/// Run the tool itself. Everything around it - hooks, truncation, outcome - is
/// the caller's, so both the success and the failure path get all of it.
fn dispatch_tool(
    name: &str,
    arguments: &str,
    is_mcp_tool: bool,
    mcp: &Arc<RwLock<McpManager>>,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, ToolCallError> {
    let result = if matches!(
        name,
        "task_plan"
            | "task_verify"
            | "job_status"
            | "job_output"
            | "job_cancel"
            | "tool_search"
            | "mcp_task_status"
            | "mcp_task_cancel"
    ) {
        let runtime = proxy.agent_runtime().ok_or("tool requires an agent task")?;
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        match name {
            "tool_search" => {
                let query = args["query"]
                    .as_str()
                    .ok_or("query required")?
                    .to_lowercase();
                if query.trim().is_empty() {
                    return Err("nonempty query required".into());
                }
                let words: Vec<_> = query.split_whitespace().collect();
                mcp.write()
                    .refresh_tools_if_expired(&|| super::task_cancelled(proxy))?;
                let definitions: Vec<_> = mcp
                    .read()
                    .tool_definitions()
                    .into_iter()
                    .filter(|d| {
                        let description =
                            format!("{} {}", d["function"]["name"], d["function"]["description"])
                                .to_lowercase();
                        words.iter().all(|word| description.contains(word))
                    })
                    .take(8)
                    .collect();
                serde_json::json!({"tools":definitions}).to_string()
            }
            "mcp_task_status" | "mcp_task_cancel" => {
                let server = args["server"].as_str().ok_or("server required")?;
                let id = args["task_id"].as_str().ok_or("task_id required")?;
                let events = {
                    let runtime = runtime.lock();
                    runtime
                        .store
                        .events(&runtime.task.id)
                        .map_err(|e| e.to_string())?
                };
                if !crate::agent::has_remote_task(&events, server, id) {
                    return Err("task handle was not created by this agent task".into());
                }
                let kind = if name == "mcp_task_status" {
                    "task_get"
                } else {
                    "task_cancel"
                };
                let result = mcp.read().task_operation(
                    server,
                    kind,
                    serde_json::json!({"taskId":id}),
                    &|| super::task_cancelled(proxy),
                )?;
                if result["status"] == "input_required" {
                    let mut runtime = runtime.lock();
                    runtime.task.status = dsh_types::agent::TaskStatus::InputRequired;
                    runtime.task.stop_reason = Some(
                        "MCP task needs user input; inspect agent show and use agent respond"
                            .into(),
                    );
                    runtime.save(None).map_err(|e| e.to_string())?;
                }
                result.to_string()
            }
            "job_status" | "job_output" | "job_cancel" => {
                let id = args["job_id"].as_str().ok_or("job_id required")?;
                let mut runtime = runtime.lock();
                if name == "job_cancel" {
                    runtime.jobs.cancel(id).map_err(|e| e.to_string())?;
                }
                runtime
                    .jobs
                    .snapshot(
                        id,
                        args["offset"].as_u64().unwrap_or(0) as usize,
                        args["limit"].as_u64().unwrap_or(4096) as usize,
                    )
                    .or_else(|error| {
                        if name == "job_cancel" {
                            return Err(error);
                        }
                        let mut archived = runtime.store.load_artifact(&runtime.task.id, id)?;
                        archived["archived"] = serde_json::json!(true);
                        archived["job_id"] = serde_json::json!(id);
                        for stream in ["stdout", "stderr"] {
                            let text = archived[stream].as_str().unwrap_or_default();
                            let start = text.ceil_char_boundary(
                                (args["offset"].as_u64().unwrap_or(0) as usize).min(text.len()),
                            );
                            let end = text.floor_char_boundary(
                                start
                                    .saturating_add(
                                        (args["limit"].as_u64().unwrap_or(4096) as usize)
                                            .min(65536),
                                    )
                                    .min(text.len()),
                            );
                            archived[stream] = serde_json::json!(&text[start..end]);
                        }
                        Ok(archived)
                    })
                    .map_err(|e| e.to_string())?
                    .to_string()
            }
            _ => crate::agent::task_tool(&mut runtime.lock(), name, &args)
                .map_err(|e| e.to_string())?,
        }
    } else if is_mcp_tool {
        if !authorize_mcp_tool(name, arguments, proxy)? {
            // A cancellation is a result, not a dispatch error: `result_failed`
            // recognises the wording and the caller reports it as a failure.
            return Ok("MCP tool execution cancelled by user.".to_string());
        }

        mcp.read().execute_tool_cancellable(
            name,
            arguments,
            &|| super::task_cancelled(proxy),
            proxy.agent_runtime().is_some(),
        )?
    } else {
        match name {
            edit::NAME => edit::run(arguments, proxy)?,
            execute::NAME => execute::run(arguments, proxy)?,
            ls::NAME => ls::run(arguments, proxy)?,
            read::NAME => read::run(arguments, proxy)?,
            replace::NAME => replace::run(arguments, proxy)?,
            search::NAME => search::run(arguments, proxy)?,
            shell_context::NAME => shell_context::run(arguments, proxy)?,
            shell_history::NAME => shell_history::run(arguments, proxy)?,
            skill::NAME => skill::run(arguments, proxy)?,
            other => return Err(format!("chat: unsupported tool `{other}`").into()),
        }
    };

    Ok(result)
}

/// Which family a tool belongs to, for a hook that wants to treat them
/// differently without pattern-matching on names.
fn tool_kind(name: &str, is_mcp_tool: bool) -> &'static str {
    if is_agent_task_tool(name) {
        "agent"
    } else if is_mcp_tool {
        "mcp"
    } else {
        "builtin"
    }
}

fn is_agent_task_tool(name: &str) -> bool {
    matches!(
        name,
        "task_plan"
            | "task_verify"
            | "job_status"
            | "job_output"
            | "job_cancel"
            | "tool_search"
            | "mcp_task_status"
            | "mcp_task_cancel"
    )
}

fn post_tool_detail(
    name: &str,
    tool_call_id: Option<&str>,
    kind: &str,
    arguments: &str,
    content: &str,
    outcome: ToolOutcome,
    elapsed: std::time::Duration,
) -> Value {
    let mut detail = hooks::tool_detail(name, tool_call_id, kind, arguments);
    // What the model is about to read, masked the same way: a hook watching for
    // a leak should see what would have leaked, not the pre-truncation text.
    let redacted = hooks::redact(content);
    let truncated = redacted.len() > HOOK_RESULT_LIMIT;
    let result = if truncated {
        dsh_openai::turn::truncate_middle(&redacted, HOOK_RESULT_LIMIT)
    } else {
        redacted
    };

    if let Value::Object(fields) = &mut detail {
        fields.insert("result".to_string(), Value::String(result));
        fields.insert("result_truncated".to_string(), Value::Bool(truncated));
        fields.insert(
            "outcome".to_string(),
            Value::String(
                match outcome {
                    ToolOutcome::Success => "success",
                    ToolOutcome::Failure => "failure",
                    ToolOutcome::OutcomeUnknown => "outcome_unknown",
                }
                .to_string(),
            ),
        );
        fields.insert(
            "duration_ms".to_string(),
            Value::from(elapsed.as_millis() as u64),
        );
    }

    detail
}

/// Ceiling on the tool result handed to a hook.
const HOOK_RESULT_LIMIT: usize = 64 * 1024;

/// Put an MCP call through the shell's own safety policy.
///
/// This used to prompt unconditionally, which meant `loose` still asked about a
/// read-only tool and "always" was unreachable - the opposite of what the same
/// call got through the shell-side AI service.
fn authorize_mcp_tool(
    name: &str,
    arguments: &str,
    proxy: &mut dyn ChatToolHost,
) -> Result<bool, String> {
    match proxy.evaluate_agent_tool(name, arguments) {
        AgentCommandVerdict::Allowed => Ok(true),
        AgentCommandVerdict::Denied(reason) => {
            Err(format!("chat: MCP tool `{name}` refused: {reason}"))
        }
        AgentCommandVerdict::Confirm(reason) => {
            let message = format!("AI wants to call MCP tool: `{name}` ({reason})");
            match proxy
                .request_agent_approval(&message)
                .map_err(|err: anyhow::Error| err.to_string())?
            {
                ApprovalDecision::Allow => Ok(true),
                ApprovalDecision::AllowAlways => {
                    let entry = proxy.agent_tool_approval_entry(name, arguments);
                    proxy.remember_agent_approval(&entry);
                    Ok(true)
                }
                ApprovalDecision::Deny => Ok(false),
            }
        }
    }
}

fn truncate_args(args: &str) -> String {
    const MAX_ARGS_LEN: usize = 80;
    if args.len() > MAX_ARGS_LEN {
        let end = args.floor_char_boundary(MAX_ARGS_LEN);
        format!("{}...", &args[..end])
    } else {
        args.to_string()
    }
}

pub(crate) fn redact_tool_arguments(args: &str) -> String {
    safety_policy::redact_sensitive_text(args)
}

/// Fields `truncate_output`'s final fallback keeps whole (up to
/// `MAX_PRESERVED_FIELD_CHARS` each) instead of dropping along with the rest
/// of a too-big result: kept in sync with what `result_failed` (below) reads
/// plus the job handle a caller polls with.
const PRESERVED_STATUS_FIELDS: &[&str] = &["exit_code", "status", "job_id"];
/// These fields are meant to be short (an exit code, an enum-like status, a
/// UUID-shaped handle); this bounds them defensively so a malformed value
/// cannot reopen the "result no longer fits" problem the fallback exists to
/// solve.
const MAX_PRESERVED_FIELD_CHARS: usize = 256;
/// However little room the preserved fields leave, the preview must still say
/// something.
const MIN_PREVIEW_CHARS: usize = 64;

fn truncate_output(output: String) -> String {
    if output.len() <= MAX_OUTPUT_LENGTH {
        return output;
    }
    if let Ok(mut value) = serde_json::from_str::<Value>(&output) {
        // Preserve handles and status fields; cutting serialized JSON makes
        // successful long-running commands impossible to poll reliably.
        fn trim(value: &mut Value) {
            match value {
                Value::String(text) if text.len() > 2048 => {
                    *text = dsh_openai::turn::truncate_middle(text, 2048);
                }
                Value::Object(values) => values.values_mut().for_each(trim),
                Value::Array(values) => values.iter_mut().for_each(trim),
                _ => {}
            }
        }
        trim(&mut value);
        if value.to_string().len() <= MAX_OUTPUT_LENGTH {
            return value.to_string();
        }
        // The trimmed value is still too big to fit; fall back to a preview.
        // `PRESERVED_STATUS_FIELDS` decide whether `result_failed` sees a
        // failure and whether a job can still be polled, so they are carried
        // over rather than dropped with the rest of the body - losing them
        // here made a failed command that produced a huge log look like a
        // Success to the model.
        let mut fallback = serde_json::json!({ "output_truncated": true });
        if let (Value::Object(source), Value::Object(target)) = (&value, &mut fallback) {
            for key in PRESERVED_STATUS_FIELDS {
                if let Some(field) = source.get(*key) {
                    // These are meant to be a short exit code, an enum-like
                    // status, or a UUID-shaped job handle - never free text.
                    // Cap defensively anyway so a malformed or hostile tool
                    // response cannot smuggle enough text through them to
                    // push the whole fallback back over MAX_OUTPUT_LENGTH,
                    // which is the exact problem this function exists to
                    // prevent.
                    let capped = match field {
                        Value::String(text) if text.len() > MAX_PRESERVED_FIELD_CHARS => {
                            Value::String(dsh_openai::turn::truncate_middle(
                                text,
                                MAX_PRESERVED_FIELD_CHARS,
                            ))
                        }
                        other => other.clone(),
                    };
                    target.insert((*key).to_string(), capped);
                }
            }
        }
        // Give the preview whatever room is left after the fields above, so
        // the total stays bounded near MAX_OUTPUT_LENGTH instead of the two
        // budgets being sized independently and simply added together.
        let scaffold_len = fallback.to_string().len();
        let preview_budget = (MAX_OUTPUT_LENGTH / 2)
            .min(MAX_OUTPUT_LENGTH.saturating_sub(scaffold_len))
            .max(MIN_PREVIEW_CHARS);
        if let Value::Object(target) = &mut fallback {
            target.insert(
                "preview".to_string(),
                Value::String(dsh_openai::turn::truncate_middle(&output, preview_budget)),
            );
        }
        return fallback.to_string();
    }
    dsh_openai::turn::truncate_middle(&output, MAX_OUTPUT_LENGTH)
}

pub(crate) fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {
                // Skip current directory components
            }
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}

pub(crate) fn tool_skills_dir() -> PathBuf {
    crate::config_paths::skills_dir()
}

fn canonicalize_or_normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

pub(crate) fn resolve_with_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut current = path.to_path_buf();
    let mut suffix = PathBuf::new();

    loop {
        if current.exists() {
            let canonical = std::fs::canonicalize(&current).map_err(|err| {
                format!(
                    "chat: failed to canonicalize path ancestor `{}`: {err}",
                    current.display()
                )
            })?;
            return Ok(if suffix.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(suffix)
            });
        }

        let name = current.file_name().ok_or_else(|| {
            format!(
                "chat: path `{}` has no existing ancestor",
                path.to_string_lossy()
            )
        })?;
        // `Path::join` on an empty path appends a separator, so building the
        // suffix from an empty `PathBuf` produced `notes.txt/` - a directory
        // name. `fs::write` then failed with ENOENT and the `edit` tool could
        // not create a file at all, which is half of what it is for.
        suffix = if suffix.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            PathBuf::from(name).join(&suffix)
        };

        if !current.pop() {
            return Err(format!(
                "chat: path `{}` has no existing ancestor",
                path.to_string_lossy()
            ));
        }
    }
}

/// The directories a tool may touch.
///
/// The project root, not just the current directory: in a workspace, running
/// `!` from `dsh-builtin/` used to put the top-level `Cargo.toml` and every
/// sibling crate out of reach, with an error message that offered no way
/// around it. The root is the nearest ancestor carrying a project marker, so
/// this widens to the repository and stops there rather than at `$HOME`.
fn allowed_tool_roots(current_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![canonicalize_or_normalize(current_dir)];
    let workspace_root = canonicalize_or_normalize(&workspace_root(current_dir));
    if !roots.contains(&workspace_root) {
        roots.push(workspace_root);
    }
    roots.push(canonicalize_or_normalize(&tool_skills_dir()));
    roots
}

/// The outermost project enclosing `current_dir`.
///
/// `find_project_root` stops at the *nearest* marker, which for a workspace
/// member is the member itself - so running `!` in `dsh-builtin/` still could
/// not see the workspace `Cargo.toml` one level up. Walking while each parent
/// is also a project extends the reach to the repository and stops there: the
/// chain breaks at the first directory that is not a project, so an unrelated
/// parent never becomes readable.
///
/// Never expands to the home directory itself, which a dotfiles repository
/// would otherwise qualify by way of its `.git`.
pub(crate) fn workspace_root(current_dir: &Path) -> PathBuf {
    let current_dir = canonicalize_or_normalize(current_dir);
    let home = dirs::home_dir().map(|home| canonicalize_or_normalize(&home));
    let too_far = |candidate: &Path| home.as_deref().is_some_and(|home| candidate == home);

    let mut root =
        canonicalize_or_normalize(&crate::project_context::find_project_root(&current_dir));

    // `find_project_root` walks ancestors, so with a dotfiles repository in
    // `$HOME` it answers `$HOME` for any directory that is not itself a
    // project - which would have put `~/.ssh` and `~/.aws` inside the sandbox.
    // Checking the starting point, not only the climb, is what stops that.
    if too_far(&root) {
        return current_dir;
    }

    while let Some(parent) = root.parent() {
        if too_far(parent) || !crate::project_context::has_project_marker(parent) {
            break;
        }
        root = parent.to_path_buf();
    }

    root
}

pub(crate) fn is_path_within_tool_roots(path: &Path, current_dir: &Path) -> bool {
    let roots = allowed_tool_roots(current_dir);
    roots.iter().any(|root| path.starts_with(root))
}

pub(crate) fn resolve_tool_path(
    path_str: &str,
    proxy: &mut dyn ChatToolHost,
) -> Result<std::path::PathBuf, String> {
    // Use shellexpand to handle ~
    let expanded = shellexpand::full(path_str)
        .map_err(|e| format!("chat: failed to expand path `{path_str}`: {e}"))?;
    let path = Path::new(expanded.as_ref());
    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };
    let resolved_path = if absolute_path.exists() {
        std::fs::canonicalize(&absolute_path).map_err(|err| {
            format!(
                "chat: failed to canonicalize path `{}`: {err}",
                absolute_path.display()
            )
        })?
    } else {
        resolve_with_existing_ancestor(&absolute_path)?
    };

    if let Some(runtime) = proxy.agent_runtime() {
        let runtime = runtime.lock();
        let grant = &runtime.task.grant;
        let state = crate::config_paths::agent_state_dir();
        if resolved_path.starts_with(&state)
            || crate::safety_policy::is_sensitive_path(&resolved_path)
        {
            return Err("agent: protected path cannot be read through task tools".into());
        }
        if grant
            .read_roots
            .iter()
            .chain(&grant.write_roots)
            .any(|root| resolved_path.starts_with(root))
            || resolved_path.starts_with(crate::config_paths::skills_dir())
        {
            return Ok(resolved_path);
        }
        return Err(
            "agent: path is outside task grants; resume with an explicit --read or --write grant"
                .into(),
        );
    }
    if is_path_within_tool_roots(&resolved_path, &current_dir) {
        return Ok(resolved_path);
    }

    Err(format!(
        "chat: path `{path_str}` resolves outside allowed directories"
    ))
}

pub(crate) fn safety_level(proxy: &mut dyn ShellProxy) -> SafetyLevel {
    proxy.safety_level()
}

pub(crate) fn reject_gitignored_path(
    path: &Path,
    base_dir: &Path,
    user_path: &str,
) -> Result<(), String> {
    match gitignore::is_gitignored(path, base_dir) {
        Ok(false) => Ok(()),
        Ok(true) => Err(format!(
            "chat: tool path `{user_path}` is ignored by .gitignore"
        )),
        Err(err) => Err(format!("chat: failed to apply .gitignore policy: {err}")),
    }
}

/// The same rule for the tools that only read, with skills exempted.
///
/// A repository that ignores `.dsh/` would otherwise have its project skills
/// advertised in the prompt and then refused by every `read_file`. The
/// exemption is read-only on purpose: the justification is "the prompt already
/// pointed the model here", which says nothing about writing. `skill_manage` is
/// the way to change a skill, and it validates the name, the path and the
/// symlinks that plain `edit` would not.
///
/// The skill roots are resolved only on the branch that would reject, because
/// `ls` calls this once per directory entry and the resolution walks ancestors.
pub(crate) fn reject_gitignored_read_path(
    path: &Path,
    base_dir: &Path,
    user_path: &str,
) -> Result<(), String> {
    match gitignore::is_gitignored(path, base_dir) {
        Ok(false) => Ok(()),
        Ok(true) if crate::chatgpt::skills::is_within_skill_root(path, base_dir) => Ok(()),
        Ok(true) => Err(format!(
            "chat: tool path `{user_path}` is ignored by .gitignore"
        )),
        Err(err) => Err(format!("chat: failed to apply .gitignore policy: {err}")),
    }
}

/// The same content check `skill_manage` runs, applied to the other two
/// tools that can reach the exact same file.
///
/// `skill_manage` validates the name, the path, the symlinks *and* (since the
/// content lint) the shape of what it writes - but `edit` and `str_replace`
/// advertise "absolute for skills" in their own schemas and were never routed
/// through any of that. Without this, deleting a skill's `description:` line
/// through `str_replace` reopened exactly the bug the content lint closed,
/// just through a different tool.
///
/// Only ever inspects the file the loader actually parses for frontmatter -
/// a folder skill's `SKILL.md`, or a bare `*.md` file that is the whole
/// skill. A bundled `references/*` file is unaffected: `lint_bundled` is
/// advisory even from `skill_manage` itself, so there is nothing to gate here.
/// Structural, not existence-based, so creating a brand-new skill this way is
/// caught the same as editing one that is already on disk.
pub(crate) fn reject_broken_skill_md(
    path: &Path,
    current_dir: &Path,
    contents: &str,
) -> Result<(), String> {
    let Some((skill_dir, _scope)) = crate::chatgpt::skills::containing_skill(path, current_dir)
    else {
        return Ok(());
    };

    let is_folder_skill_md = path.parent() == Some(skill_dir.as_path())
        && path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md");
    let is_bare_md_skill =
        path == skill_dir && path.extension().and_then(|ext| ext.to_str()) == Some("md");
    if !is_folder_skill_md && !is_bare_md_skill {
        return Ok(());
    }

    let name = if is_folder_skill_md {
        skill_dir.file_name()
    } else {
        skill_dir.file_stem()
    };
    let Some(name) = name.and_then(|name| name.to_str()) else {
        return Ok(());
    };

    let findings = crate::chatgpt::skills::lint::lint_skill_md(name, contents);
    if let Some(reason) = crate::chatgpt::skills::lint::has_rejection(&findings) {
        return Err(format!("chat: {reason}"));
    }
    Ok(())
}

/// Close the one way `edit`/`str_replace` could bypass staged review for a
/// path `skill_manage` would have queued instead of writing.
///
/// Only `SkillStaging::Always` needs this. `SkillStaging::Task` redirects an
/// agent task with no write grant for the target - and `edit`/`str_replace`
/// already stall there today through the very same `confirm_agent_action`
/// grant check `skill_manage` uses, so nothing is bypassed; it is simply not
/// unblocked the way `skill_manage`'s own write now is. `Always` is
/// different: it means a person asked to review *every* skill write, and
/// `edit`/`str_replace` writing the same file through their own ordinary
/// interactive confirmation would skip that review entirely.
pub(crate) fn reject_skill_path_while_staging_always(
    path: &Path,
    current_dir: &Path,
    proxy: &mut dyn ChatToolHost,
) -> Result<(), String> {
    if crate::chatgpt::resolve_skill_staging(proxy) != crate::chatgpt::SkillStaging::Always {
        return Ok(());
    }
    if crate::chatgpt::skills::containing_skill(path, current_dir).is_none() {
        return Ok(());
    }
    Err(
        "chat: this path belongs to a skill; use `skill_manage` so the change is queued for review like every other skill write"
            .to_string(),
    )
}

pub(crate) fn confirm_sensitive_access(
    proxy: &mut dyn ChatToolHost,
    action: &str,
    path_label: &str,
    resolved: &Path,
    reason: &str,
) -> Result<bool, String> {
    if !safety_level(proxy).requires_confirmation_for_sensitive_access() {
        return Ok(true);
    }

    confirm_agent_action(
        proxy,
        &sensitive_approval_key(action, resolved),
        &format!("AI wants to {action} sensitive content `{path_label}` ({reason})"),
    )
}

/// Ask the user about an action the agent wants to take, offering "always".
///
/// The three-way answer already existed for `execute` and for MCP calls; the
/// file tools were left on `ShellProxy::confirm_action`, whose bool cannot say
/// "always". A twenty-step edit was twenty prompts, which is how a safety gate
/// turns into a key people hold down.
///
/// `approval_key` is what an "always" answer remembers, matched exactly and
/// stored beside the command lines and `mcp:` entries in the same session list.
/// It is deliberately coarser than the message: the question names the change,
/// the key names the file, so approving one edit of a file does not have to be
/// re-answered for the next one.
pub(crate) fn confirm_agent_action(
    proxy: &mut dyn ChatToolHost,
    approval_key: &str,
    message: &str,
) -> Result<bool, String> {
    if let Some(runtime) = proxy.agent_runtime() {
        if let Some(path) = approval_key.strip_prefix("write:")
            && agent_write_granted(proxy, Path::new(path))
        {
            return Ok(true);
        }
        let mut runtime = runtime.lock();
        runtime.task.status = dsh_types::agent::TaskStatus::InputRequired;
        runtime.task.stop_reason = Some(message.to_string());
        runtime.save(None).map_err(|e| e.to_string())?;
        return Err(format!("agent: permission required: {message}"));
    }
    if proxy
        .agent_session_approvals()
        .iter()
        .any(|approved| approved == approval_key)
    {
        return Ok(true);
    }

    match proxy
        .request_agent_approval(message)
        .map_err(|err: anyhow::Error| format!("chat: confirmation failed: {err}"))?
    {
        ApprovalDecision::Allow => Ok(true),
        ApprovalDecision::AllowAlways => {
            proxy.remember_agent_approval(approval_key);
            Ok(true)
        }
        ApprovalDecision::Deny => Ok(false),
    }
}

/// Whether an agent task's `--write` grant already covers `path`, without
/// touching the task's status.
///
/// Shared by `confirm_agent_action` (which falls through to
/// `InputRequired` when this is `false`) and `skill_manage`'s staging check
/// (which falls through to staging a proposal instead), so the two can never
/// disagree about what a task's grant covers. `false` when there is no agent
/// task at all - callers that only make sense under one check that
/// themselves.
pub(crate) fn agent_write_granted(proxy: &mut dyn ChatToolHost, path: &Path) -> bool {
    proxy.evaluate_agent_file(path, true) == crate::shell_capabilities::AgentCommandVerdict::Allowed
}

/// What "always" remembers for a file the agent wants to change.
///
/// One key for `edit` and `str_replace` alike: the user is deciding about the
/// file, not about which tool happens to write it.
pub(crate) fn write_approval_key(resolved: &Path) -> String {
    format!("write:{}", resolved.display())
}

fn sensitive_approval_key(action: &str, resolved: &Path) -> String {
    format!("sensitive:{action}:{}", resolved.display())
}

pub(crate) fn sensitive_path_reason(path: &Path) -> Option<&'static str> {
    safety_policy::is_sensitive_path(path).then_some("sensitive path")
}

pub(crate) fn agent_definitions() -> Vec<Value> {
    use crate::agent::definition;
    let mut tools = vec![definition(
        "tool_search",
        "Find MCP tools by words in their name or description. Discovery does not authorize execution.",
        serde_json::json!({"query":{"type":"string"}}),
        &["query"],
    )];
    for name in ["job_status", "job_output", "job_cancel"] {
        tools.push(definition(name,"Inspect output/status or cancel an existing managed job. Never relaunch it to poll.", serde_json::json!({"job_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}}), &["job_id"]));
    }
    for name in ["mcp_task_status", "mcp_task_cancel"] {
        tools.push(definition(name,"Poll or request cancellation of a remote task created by this agent. Cancellation does not guarantee the remote action stopped.",serde_json::json!({"server":{"type":"string"},"task_id":{"type":"string"}}), &["server","task_id"]));
    }
    tools
}
/// Reads a subset of `PRESERVED_STATUS_FIELDS` (`exit_code`, `status`); a
/// field this comes to depend on must be added there too, or a truncated
/// result silently loses the one thing this function looks for.
pub(crate) fn result_failed(text: &str) -> bool {
    if text.starts_with("Error:")
        || text.starts_with("The tool reported an error:")
        || text.contains("cancelled by user")
    {
        return true;
    }
    if let Ok(result) = serde_json::from_str::<Value>(text) {
        if let Some(code) = result.get("exit_code")
            && code != &serde_json::json!(0)
            && result["status"] != "running"
        {
            return true;
        }
        if matches!(
            result["status"].as_str(),
            Some("failed" | "timed_out" | "cancelled")
        ) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type NoopProxy = TestShellProxy;

    /// A workspace member must be able to see the workspace.
    #[test]
    fn tool_roots_reach_the_project_root_from_a_subdirectory() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let member = root.join("crates/inner");
        std::fs::create_dir_all(&member).unwrap();

        assert!(is_path_within_tool_roots(&root.join("Cargo.toml"), &member));
    }

    /// The member carries its own `Cargo.toml`, so stopping at the nearest
    /// marker left the workspace file one level up out of reach.
    #[test]
    fn tool_roots_climb_past_a_member_that_is_itself_a_project() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let member = root.join("member");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\n").unwrap();

        assert!(is_path_within_tool_roots(&root.join("Cargo.toml"), &member));
    }

    /// A dotfiles repository in `$HOME` makes `find_project_root` answer
    /// `$HOME` for any plain directory beneath it. Widening to that answer put
    /// `~/.ssh` and `~/.aws` inside the sandbox of a shell started in, say,
    /// `~/scratch`.
    ///
    /// The working directory itself is a root either way - that is the original
    /// contract, and running `!` from `$HOME` has always meant that much. What
    /// must not happen is *reaching* `$HOME` from somewhere below it.
    #[test]
    fn widening_never_climbs_out_into_the_home_directory() {
        let _lock = execute::tests::env_lock();
        let fake_home = tempdir().unwrap();
        let home = std::fs::canonicalize(fake_home.path()).unwrap();
        std::fs::create_dir(home.join(".git")).unwrap();

        let scratch = home.join("scratch");
        std::fs::create_dir(&scratch).unwrap();

        // SAFETY: single-threaded under the shared env lock.
        let previous = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };
        let root = workspace_root(&scratch);
        let reaches_home = is_path_within_tool_roots(&home.join(".ssh"), &scratch);
        match previous {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }

        assert_eq!(root, scratch, "widening must stop below $HOME");
        assert!(!reaches_home, "$HOME must stay outside the sandbox");
    }

    /// Widening stops at the project, not at the home directory.
    #[test]
    fn tool_roots_do_not_reach_above_the_project() {
        let dir = tempdir().unwrap();
        let outside = std::fs::canonicalize(dir.path()).unwrap();
        let root = outside.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(outside.join("secrets.txt"), "no").unwrap();

        assert!(!is_path_within_tool_roots(
            &outside.join("secrets.txt"),
            &root
        ));
    }

    #[test]
    fn test_truncation_short() {
        let short = "Short output";
        assert_eq!(truncate_output(short.to_string()), short);
    }

    #[test]
    fn test_truncation_exact() {
        let exact = "a".repeat(MAX_OUTPUT_LENGTH);
        assert_eq!(truncate_output(exact.clone()), exact);
    }

    #[test]
    fn truncate_output_keeps_the_tail() {
        // The tail carries the compiler error / test summary the model must see.
        let long = format!("{}{}", "H".repeat(MAX_OUTPUT_LENGTH), "TAIL-MARKER");
        let truncated = truncate_output(long);

        assert!(truncated.starts_with("HHH"));
        assert!(truncated.ends_with("TAIL-MARKER"));
        assert!(truncated.contains("truncated"));
        assert!(truncated.len() < MAX_OUTPUT_LENGTH + 64);
    }

    /// The trimmed JSON can still be too big for a chatty command (five 2048
    /// char streams comfortably clear the 8192 char cap on their own), and the
    /// final fallback used to drop `exit_code`/`status` along with everything
    /// else - `result_failed` then had nothing to read and reported a failed
    /// command as a Success.
    #[test]
    fn truncate_output_final_fallback_keeps_exit_code_and_status() {
        // Each field over 2048 chars is capped to ~2048 by `trim`, so enough
        // large fields (not larger fields) are what pushes the *trimmed*
        // value itself past `MAX_OUTPUT_LENGTH` and into the final fallback.
        let huge = "E".repeat(4096);
        let output = serde_json::json!({
            "exit_code": 1,
            "status": "failed",
            "job_id": "job-1",
            "stdout": huge.clone(),
            "stderr": huge.clone(),
            "note": huge.clone(),
            "extra_a": huge.clone(),
            "extra_b": huge,
        })
        .to_string();
        assert!(output.len() > MAX_OUTPUT_LENGTH);

        let truncated = truncate_output(output);
        assert!(truncated.len() <= MAX_OUTPUT_LENGTH + 512);

        let value: Value = serde_json::from_str(&truncated).expect("still valid JSON");
        assert_eq!(value["exit_code"], Value::from(1));
        assert_eq!(value["status"], Value::from("failed"));
        assert_eq!(value["job_id"], Value::from("job-1"));
        assert_eq!(value["output_truncated"], Value::from(true));
        assert!(
            result_failed(&truncated),
            "a truncated failing result must still read as a failure"
        );
    }

    /// A malformed or hostile tool response could set `status`/`job_id` to
    /// something far longer than the short control values they are meant to
    /// hold. Carrying those fields over verbatim (as an earlier version of
    /// this fallback did) could push the whole fallback back over
    /// `MAX_OUTPUT_LENGTH` on top of the fixed preview budget - reopening the
    /// exact "result no longer fits" problem the fallback exists to solve.
    #[test]
    fn truncate_output_final_fallback_stays_bounded_even_with_oversized_status_fields() {
        let huge = "E".repeat(4096);
        let output = serde_json::json!({
            "exit_code": 1,
            "status": "failed ".repeat(1000),
            "job_id": "job-".repeat(1000),
            "stdout": huge,
        })
        .to_string();
        assert!(output.len() > MAX_OUTPUT_LENGTH);

        let truncated = truncate_output(output);
        assert!(
            truncated.len() <= MAX_OUTPUT_LENGTH + 512,
            "fallback grew unbounded: {} chars",
            truncated.len()
        );

        let value: Value = serde_json::from_str(&truncated).expect("still valid JSON");
        assert_eq!(value["exit_code"], Value::from(1));
        assert!(result_failed(&truncated));
    }

    #[test]
    fn execute_tool_call_returns_parseable_json_after_the_global_cap() {
        // The global cap runs after the tool, so it must not corrupt a
        // structured result on its way back to the model.
        //
        // `EXECUTE_TOOL_ENV_ALLOWLIST` is process-global and wins over the
        // proxy's list, so this has to hold the same lock as the tests that set
        // it or `ls` stops being allowed halfway through the run.
        let _lock = super::execute::tests::ENV_LOCK.lock().unwrap();
        let _env_guard =
            super::execute::tests::EnvGuard::set(super::execute::EXECUTE_TOOL_ENV_ALLOWLIST, "ls");

        let dir = tempdir().unwrap();
        for index in 0..400 {
            std::fs::write(dir.path().join(format!("f-{index:0>50}")), b"x").unwrap();
        }

        let mut proxy = NoopProxy {
            current_dir: std::env::current_dir().unwrap(),
            execute_allowlist: vec!["ls".to_string()],
            confirm_result: true,
            ..NoopProxy::default()
        };

        let tool_call = json!({
            "function": {
                "name": "execute",
                "arguments": format!("{{\"command\":\"ls -R {}\"}}", dir.path().display())
            }
        });

        let result = execute_tool_call(
            &tool_call,
            &Arc::new(RwLock::new(crate::chatgpt::McpManager::default())),
            &HookContext::disabled(),
            &mut proxy,
        )
        .unwrap();

        assert!(result.content.len() <= MAX_OUTPUT_LENGTH + 128);
        serde_json::from_str::<Value>(&result.content).expect("tool result must stay valid JSON");
        assert_eq!(result.outcome, ToolOutcome::Success);
    }

    #[test]
    fn tool_argument_log_redacts_secret_like_values() {
        let args =
            r#"{"path":"config.txt","contents":"API_KEY=secret Authorization: Bearer token"}"#;
        let redacted = redact_tool_arguments(args);

        assert!(redacted.contains("API_KEY=***"));
        assert!(redacted.contains("Authorization: Bearer ***"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("token"));
    }

    #[test]
    fn test_execute_tool_call_unknown_tool() {
        let mut proxy = NoopProxy::default();
        let mcp = Arc::new(RwLock::new(McpManager::load_blocking(vec![])));
        let tool_call = serde_json::json!({
            "function": {
                "name": "unknown_tool",
                "arguments": "{}"
            }
        });

        let result = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "chat: unsupported tool `unknown_tool`"
        );
    }

    /// The policy decides, not the call site. `!` used to prompt for every MCP
    /// call at every safety level, which `loose` was supposed to switch off and
    /// which made "always" unreachable.
    #[test]
    fn an_allowed_mcp_tool_runs_without_asking() {
        let mut proxy = NoopProxy {
            agent_tool_verdict: AgentCommandVerdict::Allowed,
            ..NoopProxy::default()
        };
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {"name": "mcp__test__tool", "arguments": "{}"}
        });

        // No binding is actually connected, so the call fails after the gate -
        // what matters is that the gate did not ask.
        let error =
            execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap_err();
        assert_eq!(error.outcome, ToolOutcome::Failure);
        assert_eq!(proxy.confirm_calls, 0);
    }

    #[test]
    fn a_denied_mcp_tool_is_refused_without_asking() {
        let mut proxy = NoopProxy {
            agent_tool_verdict: AgentCommandVerdict::Denied("policy says no".to_string()),
            ..NoopProxy::default()
        };
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {"name": "mcp__test__tool", "arguments": "{}"}
        });

        let err =
            execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap_err();
        assert!(err.to_string().contains("policy says no"));
        assert_eq!(proxy.confirm_calls, 0);
    }

    /// "always" was unreachable while this path used the bool `confirm_action`.
    #[test]
    fn an_always_answer_is_remembered_for_the_session() {
        let mut proxy = NoopProxy {
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..NoopProxy::default()
        };
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {"name": "mcp__test__tool", "arguments": "{}"}
        });

        let _ = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy);
        assert_eq!(proxy.agent_session_allowlist, vec!["mcp:mcp__test__tool"]);
    }

    #[test]
    fn execute_tool_call_requires_confirmation_for_mcp_tool() {
        let mut proxy = NoopProxy::default();
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {
                "name": "mcp__test__tool",
                "arguments": "{}"
            }
        });

        let result =
            execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap();

        assert_eq!(result.content, "MCP tool execution cancelled by user.");
        assert_eq!(result.outcome, ToolOutcome::Failure);
    }

    /// A hook context whose single hook prints `body` on every tool call.
    fn hook_context(dir: &tempfile::TempDir, body: &str) -> HookContext {
        // Run as `sh <path>` rather than exec'ing a file this process just
        // wrote: a concurrent test's `fork` holds the write descriptor open and
        // the kernel answers with ETXTBSY.
        let path = dir.path().join("hook.sh");
        std::fs::write(&path, format!("{body}\n")).unwrap();

        let config = format!(
            r#"{{"version":1,"hooks":[{{"id":"gatekeeper","events":["pre-tool-use","post-tool-use"],"command":["sh","{}"]}}]}}"#,
            path.display()
        );
        HookContext::with_hooks(
            hooks::config::parse(&config).expect("test hook config"),
            dir.path().to_path_buf(),
        )
    }

    /// Like `hook_context`, but with a `match` clause the caller chooses.
    fn matched_hook_context(dir: &tempfile::TempDir, matcher: &str, body: &str) -> HookContext {
        let path = dir.path().join("hook.sh");
        std::fs::write(&path, format!("{body}\n")).unwrap();

        let config = format!(
            r#"{{"version":1,"hooks":[{{"id":"gatekeeper","events":["pre-tool-use"],"match":{matcher},"command":["sh","{}"]}}]}}"#,
            path.display()
        );
        HookContext::with_hooks(
            hooks::config::parse(&config).expect("test hook config"),
            dir.path().to_path_buf(),
        )
    }

    fn execute_call(command: &str) -> Value {
        serde_json::json!({
            "id": "call_1",
            "function": {
                "name": "execute",
                "arguments": serde_json::json!({ "command": command }).to_string(),
            }
        })
    }

    /// `dsh` puts every command through `execute`, so a hook watching `rm` used
    /// to pay its timeout on every `ls`. `programs` is what makes it not.
    #[test]
    fn a_hook_matching_on_the_command_narrows_to_one_call() {
        let dir = tempdir().unwrap();
        let hooks = matched_hook_context(
            &dir,
            r#"{"tools":["execute"],"programs":["rm"]}"#,
            r#"echo '{"decision":"deny","reason":"no removals here"}'"#,
        );
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        // The tool the hook does not care about runs untouched - and never even
        // starts the hook process.
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            agent_verdict: AgentCommandVerdict::Allowed,
            ..TestShellProxy::default()
        };
        let allowed = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();
        assert_eq!(allowed.outcome, ToolOutcome::Success);

        // The one it does care about is stopped before the policy is asked.
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            agent_verdict: AgentCommandVerdict::Allowed,
            ..TestShellProxy::default()
        };
        let denied =
            execute_tool_call(&execute_call("rm -rf /tmp/x"), &mcp, &hooks, &mut proxy).unwrap();
        assert_eq!(denied.outcome, ToolOutcome::Failure);
        assert!(
            denied.content.contains("no removals here"),
            "{}",
            denied.content
        );

        // A different command through the same tool is not the hook's business.
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            agent_verdict: AgentCommandVerdict::Allowed,
            ..TestShellProxy::default()
        };
        let other = execute_tool_call(&execute_call("true"), &mcp, &hooks, &mut proxy).unwrap();
        assert_eq!(other.outcome, ToolOutcome::Success);
    }

    fn ls_call() -> Value {
        serde_json::json!({
            "id": "call_1",
            "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}
        })
    }

    #[test]
    fn pre_tool_use_deny_skips_execution() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(&dir, r#"echo '{"decision":"deny","reason":"not here"}'"#);
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: true,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert_eq!(result.outcome, ToolOutcome::Failure);
        assert!(
            result.content.contains("Blocked by hook `gatekeeper`"),
            "{}",
            result.content
        );
        assert!(result.content.contains("not here"), "{}", result.content);
    }

    /// A hook's `ask` has to reach the user even where the safety policy would
    /// have said nothing at all.
    #[test]
    fn pre_tool_use_ask_requires_approval_even_when_the_policy_allows() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(&dir, r#"echo '{"decision":"ask","reason":"double-check"}'"#);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            agent_verdict: AgentCommandVerdict::Allowed,
            confirm_counter: Some(calls.clone()),
            confirm_result: true,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(result.outcome, ToolOutcome::Success);
    }

    #[test]
    fn pre_tool_use_ask_denied_by_user_does_not_run_the_tool() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(&dir, r#"echo '{"decision":"ask","reason":"double-check"}'"#);
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            agent_verdict: AgentCommandVerdict::Allowed,
            confirm_result: false,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert_eq!(result.outcome, ToolOutcome::Failure);
        assert!(
            result.content.contains("Blocked by hook"),
            "{}",
            result.content
        );
    }

    /// The `hook:` prefix keeps this out of the box `execute` and MCP calls use,
    /// so an "always" on one never answers the other's question.
    #[test]
    fn pre_tool_use_ask_uses_its_own_approval_key() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(&dir, r#"echo '{"decision":"ask","reason":"double-check"}'"#);
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            agent_verdict: AgentCommandVerdict::Allowed,
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert!(
            proxy
                .agent_session_allowlist
                .contains(&"hook:gatekeeper:ls".to_string()),
            "{:?}",
            proxy.agent_session_allowlist
        );
        // Not the plain tool name, which is what `execute` remembers.
        assert!(!proxy.agent_session_allowlist.contains(&"ls".to_string()));
    }

    /// The tool has already run, so this cannot undo it - but the model must not
    /// read the result as a success.
    #[test]
    fn post_tool_use_deny_marks_the_result_failed() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(
            &dir,
            r#"[ "$DSH_HOOK_EVENT" = post-tool-use ] && echo '{"decision":"deny","reason":"leaked a path"}'
exit 0"#,
        );
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: true,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert_eq!(result.outcome, ToolOutcome::Failure);
        assert!(
            result.content.contains("Rejected by hook"),
            "{}",
            result.content
        );
        assert!(
            result.content.contains("leaked a path"),
            "{}",
            result.content
        );
    }

    #[test]
    fn post_tool_use_additional_context_reaches_the_model() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(
            &dir,
            r#"[ "$DSH_HOOK_EVENT" = post-tool-use ] && echo '{"additional_context":"repo policy applies"}'
exit 0"#,
        );
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: true,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert_eq!(result.outcome, ToolOutcome::Success);
        assert!(
            result.content.ends_with("repo policy applies"),
            "{}",
            result.content
        );
    }

    /// A hook that adds context on `pre-tool-use` was advertised as reaching the
    /// model, and reached nothing.
    #[test]
    fn pre_tool_use_additional_context_reaches_the_model() {
        let dir = tempdir().unwrap();
        let hooks = hook_context(
            &dir,
            r#"[ "$DSH_HOOK_EVENT" = pre-tool-use ] && echo '{"additional_context":"read-only day"}'
exit 0"#,
        );
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: true,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));

        let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

        assert!(
            result.content.starts_with("read-only day"),
            "{}",
            result.content
        );
        assert_eq!(result.outcome, ToolOutcome::Success);
    }

    /// An audit hook that pairs pre with post was left holding an unmatched open
    /// event for exactly the calls it most wants to see.
    #[test]
    fn post_tool_use_fires_for_a_failing_tool() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("seen.log");
        let hooks = hook_context(
            &dir,
            &format!("printf '%s\\n' \"$DSH_HOOK_EVENT\" >> {}", log.display()),
        );
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: true,
            ..TestShellProxy::default()
        };
        let mcp = Arc::new(RwLock::new(McpManager::default()));
        let failing = serde_json::json!({
            "id": "call_1",
            "function": {"name": "ls", "arguments": "{\"path\":\"../outside\"}"}
        });

        let error = execute_tool_call(&failing, &mcp, &hooks, &mut proxy)
            .expect_err("the path is outside the allowed roots");

        assert_eq!(error.outcome, ToolOutcome::Failure);
        let seen = std::fs::read_to_string(&log).unwrap();
        assert!(seen.contains("pre-tool-use"), "{seen}");
        assert!(seen.contains("post-tool-use"), "{seen}");
    }

    /// The exemption exists so a repository that ignores `.dsh/` can still have
    /// its project skills read. It stops there: writing one goes through
    /// `skill_manage`, which validates what plain `edit` would not.
    #[test]
    fn a_gitignored_skill_directory_is_readable_but_not_writable() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".gitignore"),
            ".dsh/
",
        )
        .unwrap();
        let skill = root.join(".dsh/skills/demo");
        std::fs::create_dir_all(&skill).unwrap();
        let file = skill.join("SKILL.md");
        std::fs::write(&file, "---\ndescription: d\n---\n").unwrap();

        assert!(reject_gitignored_read_path(&file, &root, "SKILL.md").is_ok());
        let refused = reject_gitignored_path(&file, &root, "SKILL.md")
            .expect_err("writing into an ignored directory stays refused");
        assert!(refused.contains("ignored by .gitignore"), "{refused}");
    }

    type CwdProxy = TestShellProxy;

    #[test]
    fn a_new_file_resolves_without_a_trailing_separator() {
        let dir = tempdir().unwrap();
        let mut proxy = CwdProxy {
            current_dir: dir.path().to_path_buf(),
            ..CwdProxy::default()
        };

        // `Path::join` on an empty path appends a separator, so this used to
        // come back as `notes.txt/` and every `edit` that created a file
        // failed with ENOENT.
        let resolved = resolve_tool_path("notes.txt", &mut proxy).unwrap();
        assert_eq!(resolved.file_name().unwrap(), "notes.txt");
        assert!(!resolved.to_string_lossy().ends_with('/'));

        let nested = resolve_tool_path("a/b/notes.txt", &mut proxy).unwrap();
        assert!(nested.ends_with("a/b/notes.txt"));
        assert!(!nested.to_string_lossy().ends_with('/'));
    }

    #[test]
    fn resolve_tool_path_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = CwdProxy {
            current_dir: dir.path().to_path_buf(),
            ..CwdProxy::default()
        };
        let result = resolve_tool_path("../outside.txt", &mut proxy);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_tool_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("inside")).unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();

        let mut proxy = CwdProxy {
            current_dir: base.path().to_path_buf(),
            ..CwdProxy::default()
        };
        let result = resolve_tool_path("inside/link_out/pwned.txt", &mut proxy);
        assert!(result.is_err());
    }
}
