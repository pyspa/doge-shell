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

pub(crate) mod cron;
mod edit;
pub(crate) mod execute;
mod gitignore;
mod jobs;
mod ls;
mod paths;
mod read;
mod replace;
mod safety_gates;
mod search;
mod shell_context;
mod shell_history;
pub(crate) mod skill;

#[cfg(test)]
pub(crate) use paths::is_path_within_tool_roots;
pub(crate) use paths::{
    canonicalize_or_normalize, normalize_path, resolve_tool_path, resolve_with_existing_ancestor,
    workspace_root,
};
pub(crate) use safety_gates::{
    agent_write_granted, confirm_agent_action, confirm_sensitive_access, reject_broken_skill_md,
    reject_gitignored_path, reject_gitignored_read_path, reject_skill_path_while_staging_always,
    sensitive_path_reason, write_approval_key,
};

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
        cron::definition(),
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
    // The job tools work under either entry point: a task polls its own
    // runtime, an interactive turn polls the process-wide registry. Everything
    // else here needs a task - `task_verify` records against its criteria,
    // `mcp_task_*` against its event log, and `tool_search` only earns its
    // place where MCP definitions are *not* already in the prompt.
    let result = if matches!(name, "job_status" | "job_output" | "job_cancel") {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        jobs::dispatch(name, &args, proxy)?
    } else if matches!(
        name,
        "task_plan" | "task_verify" | "tool_search" | "mcp_task_status" | "mcp_task_cancel"
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
            cron::NAME => cron::run(arguments, proxy)?,
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

/// The job tools, which both entry points carry.
///
/// `wait_ms` defaults to 0, so a caller that does not ask for it sees exactly
/// the behaviour that existed before: answer now. Asking for it turns a run of
/// polls - one API round trip each - into a single request that waits.
pub(crate) fn job_definitions() -> Vec<Value> {
    use crate::agent::definition;
    ["job_status", "job_output", "job_cancel"]
        .into_iter()
        .map(|name| {
            definition(
                name,
                "Inspect output/status or cancel an existing managed job. Never relaunch it to poll.",
                serde_json::json!({"job_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536},"wait_ms":{"type":"integer","minimum":0,"maximum":60000,"description":"Wait up to this long for the job to finish before answering."}}),
                &["job_id"],
            )
        })
        .collect()
}

pub(crate) fn agent_definitions() -> Vec<Value> {
    use crate::agent::definition;
    let mut tools = vec![definition(
        "tool_search",
        "Find MCP tools by words in their name or description. Discovery does not authorize execution.",
        serde_json::json!({"query":{"type":"string"}}),
        &["query"],
    )];
    tools.extend(job_definitions());
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
