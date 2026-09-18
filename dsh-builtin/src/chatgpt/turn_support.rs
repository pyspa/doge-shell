//! Support code around the tool-calling loop that is not the loop itself
//! (which stays in the parent module, `chat_with_tools`): what is decided
//! once before a turn starts (`TurnSetup`), one round's tool dispatch
//! (`run_tool_calls`), and turn-end bookkeeping (usage reporting, the
//! optional skill auto-archive sweep, checkpoint repair after an
//! interruption).
use super::*;
use std::collections::BTreeSet;

/// Turn-local exposure for Interactive Tool Search.
///
/// Holds only the function names `tool_search` discovered in this
/// interactive turn - never schemas, never manager state. Schemas are
/// re-resolved from [`McpManager::tool_definitions_for`] on every
/// [`build_request_tools`] call, so a disconnect or a tool-list refresh that
/// removes a tool naturally drops it from the next request instead of
/// serving a stale copy. One value lives for one user turn; the next turn
/// starts from [`Default::default`].
#[derive(Debug, Default)]
pub(super) struct InteractiveToolExposure {
    loaded_tool_names: BTreeSet<String>,
}

impl InteractiveToolExposure {
    pub(super) fn add<I>(&mut self, names: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.loaded_tool_names.extend(names);
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &String> {
        self.loaded_tool_names.iter()
    }
}

/// Everything about a turn that is decided once, before the tool-calling
/// loop starts: which skills the model may see, the fixed system prompt,
/// whether this is an agent task, session continuity, the hook context, and
/// the two budgets a turn is held to. Bundled here so `chat_with_tools`
/// reads as "resolve the turn, then run it" instead of nine separate locals
/// threaded through a 600-line function.
pub(super) struct TurnSetup {
    pub(super) skill_roots: Vec<skills::SkillRoot>,
    pub(super) prompt: SystemPrompt,
    /// Optional durable task context. `chat_with_tools` treats a turn
    /// differently in several places when this is `Some` - session
    /// continuity is disabled, an unverified `Answer` is retried, and the
    /// checkpoint/finish calls at the end of the turn only fire here.
    pub(super) runtime: Option<Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>>,
    pub(super) session_ttl: Option<Duration>,
    pub(super) scope: Option<PathBuf>,
    pub(super) hook_ctx: hooks::HookContext,
    /// Whether `peek_id` named a stored conversation at build time.
    ///
    /// `peek_id` and `take` run the same `mismatch` check over identical
    /// inputs, so a peek hit followed by `Claim::Fresh` can only mean the
    /// turn crossed the TTL while a hook ran or an approval waited - the one
    /// case where `hook_ctx` still holds the dropped conversation's id and
    /// must be refreshed before `retain_session` runs.
    pub(super) peeked_session: bool,
    pub(super) turn_token_budget: Option<u64>,
    /// Snapshot of `skills::pending::staged_this_process()` before this turn,
    /// so the turn's own count is the difference read afterward.
    pub(super) staged_before_turn: usize,
}

impl TurnSetup {
    /// A configuration that cannot be read stops the turn (via `?`).
    /// Continuing without the hooks would silently drop checks the user
    /// believes are running.
    pub(super) fn build(
        operator_prompt: Option<String>,
        language: Option<String>,
        mcp_manager: &Arc<RwLock<McpManager>>,
        proxy: &mut dyn ChatToolHost,
    ) -> Result<Self, String> {
        // A previous turn's interrupt must not cancel this one.
        clear_turn_cancelled();

        let cwd = proxy.get_current_dir().ok();
        let mut skill_roots =
            skills::skill_roots(cwd.as_deref(), resolve_project_skills_enabled(proxy));
        gate_project_skills(&mut skill_roots, proxy);

        // Build System Prompt (fixed for the session)
        let prompt =
            build_system_prompt(operator_prompt, language, &mcp_manager.read(), &skill_roots);

        let runtime = proxy.agent_runtime();
        let session_ttl = if runtime.is_some() {
            None
        } else {
            resolve_session_ttl(proxy)
        };
        // Only computed when something will actually read it: an agent turn's
        // `session_ttl` is always `None`, so paying for `conversation_scope`'s
        // canonicalize-and-ancestor-walk there would buy nothing.
        let scope = session_ttl
            .is_some()
            .then(|| conversation_scope(cwd.as_deref()))
            .flatten();

        let mut hook_ctx = hooks::HookContext::load(proxy)?;
        // Learned without consuming the conversation: `take` is destructive, and
        // a hook that refuses this prompt must leave the previous turn intact.
        let carried_session = session::peek_id(session_ttl, &prompt.identity, scope.as_deref());
        let peeked_session = carried_session.is_some();
        hook_ctx.set_session_id(
            carried_session
                .clone()
                .unwrap_or_else(|| hook_ctx.new_session_id()),
        );

        let turn_token_budget = resolve_turn_token_budget(proxy);
        // Snapshot before the turn runs: `staged_this_process` never resets, so
        // this turn's own count is the difference read after it.
        let staged_before_turn = skills::pending::staged_this_process();

        Ok(Self {
            skill_roots,
            prompt,
            runtime,
            session_ttl,
            scope,
            hook_ctx,
            peeked_session,
            turn_token_budget,
            staged_before_turn,
        })
    }
}

/// Fixed per-turn tool halves for `chat_with_tools`.
///
/// Returns the interactive base (`Some` for `!` turns: the unconditional
/// builtins) alongside the agent accumulator (`tools`, grown by
/// `run_tool_calls`). MCP definitions are deliberately NOT fixed here for
/// interactive turns: `build_request_tools` rebuilds them from current
/// exposure before every request, so an `mcp_load_group` call reaches the
/// very next iteration. Caching them here would freeze the first iteration's
/// exposure for the rest of the turn. Agent turns keep accumulating into the
/// vec instead (their `tool_search` discoveries live only there, not in
/// manager state), so the two paths must not share one construction.
pub(super) fn split_turn_tool_bases(
    mcp_manager: &Arc<RwLock<McpManager>>,
    is_agent: bool,
) -> (Option<Vec<Value>>, Vec<Value>) {
    if is_agent {
        let mut tools = tool::build_tools();
        {
            let mcp = mcp_manager.read();
            tools.extend(tool::mcp_turn_definitions(&mcp, false));
        }
        tools.extend(crate::agent::definitions());
        tools.extend(tool::agent_definitions());
        (None, tools)
    } else {
        (Some(tool::build_tools()), Vec::new())
    }
}

/// Tools for one LLM request.
///
/// Interactive turns rebuild the MCP part from current exposure every
/// iteration: `mcp_load_group` flips the toggle inside `McpManager` during
/// `run_tool_calls`, and the next pass here picks the newly active schemas
/// up. Turn-local `tool_search` hits are re-resolved here as well, so a
/// disconnect or refresh that removes a tool drops it from the next request.
/// The read lock covers just this construction, never the LLM request
/// itself. Agent turns reuse the accumulated vec instead, so their
/// `tool_search` discoveries survive across iterations.
///
/// The order matches the pre-lazy-loading layout - builtins, MCP, then the
/// job tools - so provider-side prefix caches see the same shape as before.
/// An interactive request is the fixed builtin base plus the rebuilt MCP
/// exposure (meta tools plus currently active group schemas) plus the
/// individually searched schemas plus the job tools.
///
/// `accumulated` (the vec `run_tool_calls` grows) is read on the agent path
/// only; on the interactive path that function never grows it, so it stays
/// an unused placeholder that keeps one shared call site.
pub(super) fn build_request_tools(
    interactive_base: &Option<Vec<Value>>,
    accumulated: &[Value],
    mcp_manager: &Arc<RwLock<McpManager>>,
    interactive_exposure: &InteractiveToolExposure,
) -> Vec<Value> {
    if let Some(base) = interactive_base {
        let mcp = mcp_manager.read();
        let mut current = base.clone();
        current.extend(tool::mcp_turn_definitions(&mcp, true));
        let searched_names: Vec<String> = interactive_exposure.names().cloned().collect();
        if !searched_names.is_empty() {
            extend_unique_tool_definitions(&mut current, mcp.tool_definitions_for(&searched_names));
        }
        current.extend(tool::job_definitions());
        current
    } else {
        accumulated.to_vec()
    }
}

/// Append definitions the target does not already carry, keyed on
/// `definition["function"]["name"]`. The first occurrence wins, so an
/// already-active group schema keeps its position when Tool Search
/// rediscovers the same tool.
fn extend_unique_tool_definitions(
    target: &mut Vec<Value>,
    definitions: impl IntoIterator<Item = Value>,
) {
    for definition in definitions {
        let name = definition["function"]["name"].clone();
        if !target.iter().any(|known| known["function"]["name"] == name) {
            target.push(definition);
        }
    }
}

/// Names `tool_search` made callable: `function.name == "tool_search"` with
/// a successful outcome and a well-formed `{"results":[{"name": ...}]}` body.
/// Anything else - failure, another tool, unparsable output - yields nothing,
/// so agent and interactive turns share one parsing rule.
///
/// Hook `additional_context` notes are prepended/appended to the same content
/// (`execute_tool_call`), so a direct parse can fail even though the embedded
/// search result is intact. Fall back to trying each blank-line-separated
/// chunk: only a chunk shaped like a search result contributes names.
fn tool_search_result_names(
    tool_call: &Value,
    outcome: crate::agent::ToolOutcome,
    result: &str,
) -> Vec<String> {
    if outcome != crate::agent::ToolOutcome::Success {
        return Vec::new();
    }
    if tool_call["function"]["name"] != "tool_search" {
        return Vec::new();
    }
    if let Some(names) = search_result_names_in(result) {
        return names;
    }
    for chunk in result.split("\n\n") {
        let chunk = chunk.trim();
        if chunk.len() == result.len() {
            continue;
        }
        if let Some(names) = search_result_names_in(chunk) {
            return names;
        }
    }
    Vec::new()
}

fn search_result_names_in(text: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let results = value.get("results")?.as_array()?;
    Some(
        results
            .iter()
            .filter_map(|hit| hit["name"].as_str())
            .map(str::to_owned)
            .collect(),
    )
}

/// Runs one round's tool calls against the shell: `before_tool`/`after_tool`
/// bookkeeping for a durable task (when there is one), dispatch through
/// `execute_tool_call`, and appending each result to `manager`. Growing
/// `tools` here is an agent-turn mechanism only: `tool_search` discoveries
/// and `mcp_load_group` activations accumulate in that vec because an agent
/// turn's per-request view is the accumulated vec, not a fresh exposure read.
/// Interactive turns instead record `tool_search` hits by name in
/// `interactive_exposure` and rebuild their MCP definitions from current
/// exposure before every request (see `chat_with_tools`), so a toggle flip
/// from `mcp_load_group` dispatch needs no merge here.
///
/// Takes just the two pieces of `TurnSetup` this round actually reads
/// (`runtime`, `hook_ctx`), not the whole struct - so a change to
/// `TurnSetup`'s other fields (skill roots, prompt, session ttl/scope,
/// budgets) is visibly unrelated to this function.
///
/// A failing tool call becomes an error message the model reads next round,
/// not a stopped turn - only `before_tool`/`after_tool` failing (the durable
/// task ledger itself is broken) propagates out as `Err`. The one exception
/// is the missing-plan guard: a mutation before `task_plan` is a recoverable
/// ordering error, returned as a tool result so the next round can record a
/// plan and retry. A model that never records a plan keeps hitting this
/// rejection until `MAX_TOOL_ITERATIONS`/budget ends the turn; that bound is
/// the backstop, not a per-call escalation.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_tool_calls(
    tool_calls: &[Value],
    mcp_manager: &Arc<RwLock<McpManager>>,
    runtime: Option<&Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>>,
    hook_ctx: &hooks::HookContext,
    proxy: &mut dyn ChatToolHost,
    manager: &mut ConversationManager,
    tools: &mut Vec<Value>,
    interactive_exposure: &mut InteractiveToolExposure,
) -> Result<(), String> {
    for tool_call in tool_calls {
        let tool_call_id = tool_call
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if let Some(runtime) = runtime
            && let Err(e) = runtime.lock().before_tool(
                tool_call,
                serde_json::to_value(&*manager).map_err(|e| e.to_string())?,
            )
        {
            // A missing plan/criteria is a model ordering error, not a
            // broken task ledger: feed it back as a tool result so the
            // next round can call `task_plan` and retry. Anything else
            // (stopped task, exhausted budget, unusable store) still ends
            // the turn as `Err`.
            if crate::agent::is_missing_plan_error(&e) {
                let message = e.to_string();
                manager.add_message(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": format!("Error: {message}. Call task_plan with plan and criteria first, then retry the operation."),
                    }));
                continue;
            }
            return Err(e.to_string());
        }
        let execution = match execute_tool_call(tool_call, mcp_manager, hook_ctx, proxy) {
            Ok(execution) => execution,
            Err(error) => tool::ToolExecution {
                content: format!(
                    "Error: {error}\nPlease analyze the error and retry with corrected arguments."
                ),
                outcome: error.outcome,
            },
        };
        let mut tool_result = execution.content;
        // Tool-level loading: the compact result names the hits, resolved
        // without flipping any group toggle so the next request can call
        // exactly these tools. A hit removed between search and load resolves
        // to nothing and is skipped; calling it would report the error instead.
        let discovered = tool_search_result_names(tool_call, execution.outcome, &tool_result);
        // Agent turns only: their per-request view is this accumulated vec,
        // so a freshly activated group must be merged in to take effect the
        // same turn. Interactive turns skip this - they rebuild from current
        // exposure before every request, which already reflects the toggle
        // flip above. Reading the group's definitions here duplicates that
        // rebuild for no gain and would reintroduce chat-loop state tracking.
        if runtime.is_some() {
            merge_activated_group_tools(tool_call, execution.outcome, mcp_manager, tools);
            if !discovered.is_empty() {
                extend_unique_tool_definitions(
                    tools,
                    mcp_manager.read().tool_definitions_for(&discovered),
                );
            }
        } else if !discovered.is_empty() {
            // Interactive turns keep names only: `build_request_tools`
            // re-resolves them every iteration, so a disconnect or refresh
            // drops what no longer exists instead of serving a stale schema.
            interactive_exposure.add(discovered);
        }

        if let Some(runtime) = runtime {
            let sequence = runtime
                .lock()
                .after_tool(tool_call, &tool_result, execution.outcome)
                .map_err(|e| e.to_string())?;
            tool_result.push_str(&format!("\n[task event {sequence}]"));
        }
        // Add tool result to history buffer
        manager.add_message(json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": tool_result,
        }));
    }
    Ok(())
}

/// Offer a freshly activated group's schemas on the next model request of an
/// agent turn.
///
/// Agent-only: interactive turns rebuild from current exposure instead (see
/// `run_tool_calls`), so this never runs for them. Unlike `tool_search`
/// (which both turns resolve per-tool), group activation is the one way
/// hidden schemas join an in-flight agent turn. Matching on dispatch success
/// plus the call's own arguments - rather than the result text, which
/// truncation and hook notes can reshape - keeps this immune to everything
/// downstream of dispatch. `already_active` merges as a no-op through the
/// dedup below, so repeat loads cost a round but never duplicate a schema;
/// the turn's `MAX_TOOL_ITERATIONS` bound is the backstop, not a per-load cap.
fn merge_activated_group_tools(
    tool_call: &Value,
    outcome: crate::agent::ToolOutcome,
    mcp_manager: &Arc<RwLock<McpManager>>,
    tools: &mut Vec<Value>,
) {
    if outcome != crate::agent::ToolOutcome::Success {
        return;
    }
    if tool_call["function"]["name"] != tool::mcp_groups::LOAD_NAME {
        return;
    }
    let group = tool_call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .and_then(Value::as_str)
        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
        .and_then(|args| {
            args.get("group")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(group) = group else {
        return;
    };
    extend_unique_tool_definitions(tools, mcp_manager.read().group_tool_definitions(&group));
}

/// Record a terminal task failure when the turn cannot proceed far enough
/// to reach the shared epilogue in `chat_with_tools`.
///
/// Pre-loop failures (an unreadable checkpoint, an unreadable event log)
/// exit the turn closure via `?` before the loop - and therefore before the
/// epilogue's `runtime.finish` - which used to leave the task `Running`
/// with only `dsh/src/agent.rs`'s generic fallback as its `stop_reason`.
/// Calling this before returning keeps the specific reason.
pub(super) fn finish_task_silently(
    runtime: Option<&Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>>,
    reason: String,
) {
    if let Some(runtime) = runtime {
        let _ = runtime.lock().finish(false, Some(reason));
    }
}

/// Print what this turn cost, so context changes can be judged.
pub(super) fn report_turn_usage(turn: &usage::TokenUsage) {
    if turn.is_empty() {
        return;
    }
    eprintln!("\x1b[2mtokens: {}\x1b[0m", turn.summary_line());
}

/// Archive stale, agent-written, unpinned personal skills, when
/// `AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS` says to. Off unless set to a positive
/// integer - this shell does not prune anything on its own by default.
pub(super) fn maybe_auto_archive_skills(proxy: &mut dyn ChatToolHost) {
    let Some(days) = resolve_setting(proxy, SKILL_AUTO_ARCHIVE_DAYS_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|days| *days > 0)
    else {
        return;
    };
    let archived = skills::usage::sweep(skills::usage::now_ms(), days);
    if archived > 0 {
        skills::clear_skills_fragment_cache();
    }
}

/// Set for the rest of a turn once anything observes a Ctrl-C.
///
/// `ShellProxy::is_canceled` reads a flag and *clears* it (`check_and_clear_sigint`),
/// so whichever poller happens to look first consumes the interrupt and every
/// other check that turn sees `false`. That already meant a Ctrl-C during
/// `execute` killed the command but left the loop running; with managed jobs
/// polling as well there are more places to lose it. Latching here keeps the
/// question "was this turn interrupted?" answerable by all of them.
///
/// One turn is one thread, the same assumption `hooks`' re-entry guard makes.
static TURN_CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn clear_turn_cancelled() {
    TURN_CANCELLED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// The one place a turn asks whether it has been interrupted.
pub(super) fn task_cancelled(proxy: &dyn ChatToolHost) -> bool {
    if proxy.is_canceled() {
        TURN_CANCELLED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    TURN_CANCELLED.load(std::sync::atomic::Ordering::SeqCst)
        || proxy
            .agent_runtime()
            .is_some_and(|runtime| runtime.lock().stopped())
}

pub(super) fn repair_interrupted_tool_calls(
    manager: &mut ConversationManager,
    events: &[dsh_types::agent::TaskEvent],
) {
    let mut pending = Vec::new();
    for message in &manager.buffer {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if let Some(id) = call["id"].as_str() {
                    pending.push(id.to_string());
                }
            }
        }
        if let Some(id) = message["tool_call_id"].as_str() {
            pending.retain(|value| value != id);
        }
    }
    for id in pending {
        let recorded = events
            .iter()
            .rev()
            .find(|event| event.kind == "tool_result" && event.data["call"]["id"] == id);
        let content = recorded.map(|event| format!("{}\n[task event {}]", event.data["result"].as_str().unwrap_or_default(), event.sequence))
            .unwrap_or_else(|| "Interrupted before the result was recorded. Do not replay. Inspect actual state; the user's reconciliation is recorded in task progress.".into());
        manager.add_message(json!({"role":"tool","tool_call_id":id,"content":content}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search_call() -> Value {
        json!({"function": {"name": "tool_search", "arguments": "{}"}})
    }

    const SEARCH_JSON: &str =
        r#"{"query":"x","count":1,"results":[{"name":"mcp__github__search_issues"}]}"#;

    #[test]
    fn search_names_parse_plain_result() {
        assert_eq!(
            tool_search_result_names(
                &search_call(),
                crate::agent::ToolOutcome::Success,
                SEARCH_JSON
            ),
            vec!["mcp__github__search_issues".to_string()]
        );
    }

    /// Hook `additional_context` notes wrap the same JSON with blank-line
    /// separated prose; discovery must survive the decoration.
    #[test]
    fn search_names_survive_hook_context_notes() {
        let decorated = format!("policy reminder\n\n{SEARCH_JSON}\n\naudit note");
        assert_eq!(
            tool_search_result_names(
                &search_call(),
                crate::agent::ToolOutcome::Success,
                &decorated
            ),
            vec!["mcp__github__search_issues".to_string()]
        );
    }

    #[test]
    fn search_names_reject_non_success_or_other_tools_or_garbage() {
        let other = json!({"function": {"name": "mcp_load_group", "arguments": "{}"}});
        assert!(
            tool_search_result_names(
                &search_call(),
                crate::agent::ToolOutcome::Failure,
                SEARCH_JSON
            )
            .is_empty()
        );
        assert!(
            tool_search_result_names(&other, crate::agent::ToolOutcome::Success, SEARCH_JSON)
                .is_empty()
        );
        assert!(
            tool_search_result_names(
                &search_call(),
                crate::agent::ToolOutcome::Success,
                "not json"
            )
            .is_empty()
        );
    }
}
