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
mod tests;
