//! Support code around the tool-calling loop that is not the loop itself
//! (which stays in the parent module, `chat_with_tools`): what is decided
//! once before a turn starts (`TurnSetup`), one round's tool dispatch
//! (`run_tool_calls`), and turn-end bookkeeping (usage reporting, the
//! optional skill auto-archive sweep, checkpoint repair after an
//! interruption).
use super::*;

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

/// Runs one round's tool calls against the shell: `before_tool`/`after_tool`
/// bookkeeping for a durable task (when there is one), dispatch through
/// `execute_tool_call`, and appending each result to `manager`. Growing
/// `tools` here (rather than back in `chat_with_tools`) is what lets
/// `tool_search` discoveries take effect the same round they are found.
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
pub(super) fn run_tool_calls(
    tool_calls: &[Value],
    mcp_manager: &Arc<RwLock<McpManager>>,
    runtime: Option<&Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>>,
    hook_ctx: &hooks::HookContext,
    proxy: &mut dyn ChatToolHost,
    manager: &mut ConversationManager,
    tools: &mut Vec<Value>,
) -> Result<(), String> {
    for tool_call in tool_calls {
        let tool_call_id = tool_call
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if let Some(runtime) = runtime {
            if let Err(e) = runtime.lock().before_tool(
                tool_call,
                serde_json::to_value(&*manager).map_err(|e| e.to_string())?,
            ) {
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

        if let Some(runtime) = runtime {
            let sequence = runtime
                .lock()
                .after_tool(tool_call, &tool_result, execution.outcome)
                .map_err(|e| e.to_string())?;
            if tool_call["function"]["name"] == "tool_search"
                && let Ok(result) = serde_json::from_str::<Value>(&tool_result)
                && let Some(found) = result["tools"].as_array()
            {
                for definition in found {
                    if !tools
                        .iter()
                        .any(|d| d["function"]["name"] == definition["function"]["name"])
                    {
                        tools.push(definition.clone());
                    }
                }
            }
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
