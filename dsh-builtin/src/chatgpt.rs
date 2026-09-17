//! `!` chat: the synchronous agent loop `dsh/src/shell/eval.rs` calls into,
//! and the `chat_*` builtins that configure it. `chat_with_tools` (the
//! tool-calling loop itself, ~550 lines) stays in this file - its meaning
//! (iteration limits, where reflection runs, hook firing order) is the
//! contract `ai-architecture.md` §2 documents and must not drift; everything
//! decided once before the loop, or read only after it, moved out into the
//! modules below.
use super::ShellProxy;
use crate::shell_capabilities::ChatToolHost;
use dsh_openai::turn::{self, TurnOutcome, extract_message_content, interpret_response};
use dsh_openai::{
    CANCELLED_MESSAGE, ChatClient, ChatGptClient, ChatRequestOptions, OpenAiConfig,
    is_ctrl_c_cancelled, usage,
};
use dsh_types::{Context, ExitStatus};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod commands;
mod context;
mod conversation;
mod prompt;
mod settings;
mod turn_support;
mod ui;

pub use commands::*;
use context::*;
use conversation::*;
use prompt::*;
pub use settings::response_language;
use settings::*;
pub(crate) use settings::{SkillStaging, resolve_project_skills_enabled, resolve_skill_staging};
use turn_support::*;
use ui::*;

mod mcp;
pub use mcp::{
    LEGACY_SSE_UNSUPPORTED_MESSAGE, McpConnectionStatus, McpManager, McpRuntimeStateSnapshot,
    McpServerStatus,
};
pub(crate) mod tool;

use tool::{build_tools, execute_tool_call};

mod jobs;
pub use jobs::shutdown as chat_jobs_shutdown;

mod session;

pub(crate) mod hooks;
mod reflect;
pub(crate) mod skills;
use skills::{SkillRoot, SkillsManager};

/// A tool call that changes state outside the conversation, mirroring the
/// mutation set `AgentRuntime::before_tool` gates on `task_plan`.
fn is_mutating_tool_call(call: &Value) -> bool {
    let name = call
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(name, "edit" | "str_replace" | "execute" | "skill_manage") || name.starts_with("mcp__")
}

/// Sent once when `AI_CHAT_VERIFY_AFTER_MUTATION` is on and a mutating `!`
/// turn tries to finish on its first answer. The second answer is always
/// accepted, so this costs at most one extra round trip.
const VERIFY_AFTER_MUTATION_NUDGE: &str = "You ran mutating tool(s) this turn. Briefly state what you checked to verify the result (command output, file content, or test). If you have not verified yet, run the checks now instead of finishing.";

#[allow(clippy::too_many_arguments)]
fn chat_with_tools(
    client: &dyn ChatClient,
    user_input: &str,
    operator_prompt: Option<String>,
    language: Option<String>,
    temperature: Option<f64>,
    model_override: Option<String>,
    mcp_manager: &Arc<RwLock<McpManager>>,
    mut stream_sink: Option<&mut StreamSink>,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    let mut setup = TurnSetup::build(operator_prompt, language, mcp_manager, proxy)?;

    // Everything below runs inside a closure so that the tail - the usage flush
    // and `response-complete` - is reached on every exit, not only the happy
    // one. The same pre/post asymmetry was fixed at the tool layer; a hook that
    // refuses the prompt, a checkpoint that will not deserialize, or a failed
    // `before_tool` all used to leave `session-start` with no matching end.
    let mut iterations = 0usize;
    let mut turn_tokens = (0u64, 0u64, 0u64);
    let outcome: Result<String, String> = (|| {
        // Set when this turn continues a stored conversation, so the final
        // `store` below can keep the idle clock from restarting if this turn
        // fails and is rewound. Fully local to this closure - nothing after
        // it reads this.
        let mut carried_stored_at: Option<Instant> = None;
        let submitted = setup.hook_ctx.fire(
            hooks::HookEvent::UserPromptSubmit,
            hooks::HookSubject::none(),
            || {
                json!({
                    "prompt": hooks::redact(user_input),
                    "prompt_chars": user_input.chars().count(),
                })
            },
            &|| proxy.is_canceled(),
        );
        if let Some((hook, reason)) = submitted.denied() {
            // `?`, not `return`: `return` would leave this function entirely,
            // skipping the usage flush and `response-complete` below. `?`
            // only exits this turn closure, so the shared tail still runs.
            Err::<(), String>(format!("chat: blocked by hook `{hook}`: {reason}"))?;
        }
        if let Some((hook, reason)) = submitted.asked()
            && !tool::confirm_agent_action(
                proxy,
                &hooks::approval_key(hook, "user-prompt-submit"),
                &format!("hook `{hook}` flagged this request: {reason}"),
            )?
        {
            Err::<(), String>(format!("chat: blocked by hook `{hook}`: {reason}"))?;
        }

        // `@name` is the user naming a skill outright. Resolved against the
        // gated roots, so a repository whose skills were declined cannot be
        // reached this way either.
        let (mentioned, user_input) = resolve_skill_mentions(user_input, &setup.skill_roots);

        // Continue the previous conversation when it still applies, so a follow-up
        // question does not re-explore the repository from scratch.
        let mut manager = match session::take(
            setup.session_ttl,
            &setup.prompt.identity,
            setup.scope.as_deref(),
        ) {
            session::Claim::Continued(carried) => {
                let age = carried.stored_at.elapsed();
                let soon = setup
                    .session_ttl
                    .is_some_and(|ttl| session::expiry_soon(ttl, carried.stored_at));
                eprintln!(
                    "\x1b[2msession: continuing {} ({} message(s), {}s old){}\x1b[0m",
                    carried.id,
                    carried.manager.buffer.len(),
                    age.as_secs(),
                    if soon { " - expires soon" } else { "" },
                );
                carried_stored_at = Some(carried.stored_at);
                let mut manager = carried.manager;
                // The carried conversation was pinned with the skills list as it
                // stood then. Re-render it so a skill written last turn is visible
                // this turn without discarding the conversation.
                set_system_prompt(&mut manager, &setup.prompt.text);
                // Recorded before this turn's own messages are added, so a
                // failed turn can be rewound to exactly this point.
                manager.mark_turn_start();
                manager.add_message(json!({ "role": "user", "content": user_input }));
                manager
            }
            session::Claim::Fresh(reason) => {
                // Refresh only when the build-time peek had named a stored
                // conversation: peek and take share the same mismatch check
                // over identical inputs, so a peek hit followed by `Fresh`
                // can only mean this turn crossed the TTL while a hook ran
                // or an approval waited, leaving `hook_ctx` holding an id
                // `take` just dropped. Refresh it so `retain_session` below
                // cancels the orphaned jobs instead of keeping them. A
                // peek miss means the mismatch was already known at build
                // (identity/scope/age) and `hook_ctx` is already fresh -
                // replacing it again would hand `UserPromptSubmit` and
                // `SessionStart` two different ids in one turn. (For a task
                // `new_session_id` is the stable task id, so this is a no-op
                // there.)
                if reason.is_some() && setup.peeked_session {
                    let fresh = setup.hook_ctx.new_session_id();
                    setup.hook_ctx.set_session_id(fresh);
                }
                // Retained even when there is no reason to report: with the
                // TTL disabled every turn is `Fresh(None)` under a new id,
                // and skipping this left the previous turn's jobs
                // unreachable (`store` is a no-op without a TTL, and the
                // epilogue's `cancel_session(new_id)` misses the old ones).
                // Only a `!` turn owns that registry - a task's jobs
                // live in its own `AgentRuntime`.
                let orphaned = if setup.runtime.is_none() {
                    jobs::retain_session(setup.hook_ctx.session_id())
                } else {
                    0
                };
                if let Some(reason) = &reason {
                    let note = if orphaned > 0 {
                        format!("; {orphaned} job(s) cancelled")
                    } else {
                        String::new()
                    };
                    eprintln!("\x1b[2msession: new conversation ({reason}){note}\x1b[0m");
                }
                // A new conversation starts exactly here, which is what
                // `session-start` means. Observation only: its answer is ignored.
                //
                // A task with a checkpoint is a *resumption*: `session_ttl` is
                // `None` for tasks so `take` always misses, which fired
                // `session-start` again on every `agent resume` - with the same
                // session id, so a hook doing per-session setup ran twice.
                let resuming = setup
                    .runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.lock().task.checkpoint.is_some());
                if !resuming {
                    setup.hook_ctx.fire(
                        hooks::HookEvent::SessionStart,
                        hooks::HookSubject::none(),
                        || {
                            json!({
                                "source": if setup.runtime.is_some() { "agent" } else { "chat" },
                                "streaming": stream_sink.is_some(),
                            })
                        },
                        &hooks::never_cancelled,
                    );
                }
                ConversationManager::new(
                    json!({ "role": "system", "content": setup.prompt.text.clone() }),
                    // First User Input (Pinned - the original goal)
                    json!({ "role": "user", "content": user_input }),
                )
            }
        };
        if let Some(runtime) = &setup.runtime {
            let saved = runtime.lock().task.clone();
            if let Some(checkpoint) = &saved.checkpoint {
                manager = serde_json::from_value(checkpoint.clone()).map_err(|e| {
                    let reason = format!("invalid task checkpoint: {e}");
                    finish_task_silently(setup.runtime.as_ref(), reason.clone());
                    reason
                })?;
                // Restore protocol balance without re-executing any tool call.
                // Bound first: the lock guard must be dropped before the
                // `map_err` below runs, because `finish_task_silently`
                // locks the same mutex and `parking_lot::Mutex` is not
                // reentrant.
                let events = runtime
                    .lock()
                    .store
                    .events(&saved.id)
                    .map_err(|e| e.to_string());
                repair_interrupted_tool_calls(
                    &mut manager,
                    &events.inspect_err(|reason| {
                        finish_task_silently(setup.runtime.as_ref(), reason.clone());
                    })?,
                );
                set_system_prompt(&mut manager, &setup.prompt.text);
            }
        }
        // The interactive registry belongs to `!` alone. A task polls
        // `runtime.jobs`, so naming its session here - or telling it about jobs
        // whose ids resolve to nothing in its own runtime - would only send it
        // after handles it cannot use.
        let owns_chat_jobs = setup.runtime.is_none();
        if owns_chat_jobs {
            // Jobs started from here on belong to this conversation, so a later
            // turn that starts a different one knows to clean them up.
            jobs::set_session(setup.hook_ctx.session_id());
        }

        for text in &mentioned {
            manager.add_message(json!({ "role": "system", "content": text }));
        }
        // Deliberately a message rather than part of the environment snapshot:
        // that is cached on `(cwd, .git/HEAD mtime)` and a value changing every
        // second would defeat the cache for everything else in it.
        if owns_chat_jobs && let Some(note) = jobs::carried_notice() {
            manager.add_message(json!({ "role": "system", "content": note }));
        }
        // A hook that answered `user-prompt-submit` with `additional_context` is
        // telling the model something about this request, so it lands next to it.
        if let Some(note) = submitted.context_note() {
            manager.add_message(json!({ "role": "system", "content": note }));
        }
        manager.set_prompt_token_budget(resolve_prompt_token_budget(proxy));
        if setup.runtime.is_none() {
            manager.begin_turn();
        }

        let mut tools = build_tools();
        {
            let mcp = mcp_manager.read();
            if setup.runtime.is_none() && !mcp.is_empty() {
                tools.extend(mcp.tool_definitions());
            }
        }
        if setup.runtime.is_some() {
            tools.extend(crate::agent::definitions());
            tools.extend(tool::agent_definitions());
        } else {
            // Only the job tools: `tool_search` would be a second way to reach
            // MCP definitions that are already in this prompt in full, and the
            // task tools record against a task that does not exist here.
            tools.extend(tool::job_definitions());
        }
        iterations = 0;
        let turn_started = Instant::now();
        let mut unverified_answers = 0;
        let verify_after_mutation = resolve_verify_after_mutation(proxy);
        let mut mutating_calls = 0usize;
        let mut verification_nudged = false;
        let mut dynamic_context = DynamicContext::default();
        // Rounds where the model produced neither a tool call nor an answer.
        let mut stalled_rounds = 0usize;

        let outcome = 'agent: loop {
            if let Some(runtime) = &setup.runtime {
                let snapshot = match serde_json::to_value(&manager) {
                    Ok(snapshot) => snapshot,
                    Err(e) => break Err(e.to_string()),
                };
                let mut runtime = runtime.lock();
                if let Err(e) = runtime.checkpoint(snapshot, manager.turn_usage.total_tokens()) {
                    break Err(e.to_string());
                }
                if runtime.stopped() {
                    break Err("agent: task stopped or time budget exhausted".into());
                }
            }
            if task_cancelled(proxy) {
                break Err(CANCELLED_MESSAGE.to_string());
            }
            iterations += 1;
            // A hundred rounds is otherwise a wall of `[Tool]` lines with no
            // sense of how far in they are or what is still running. Costs no
            // tokens: it never reaches the model.
            let running_jobs = if owns_chat_jobs {
                jobs::running_count()
            } else {
                0
            };
            eprintln!(
                "\x1b[2m[{iterations}/{MAX_TOOL_ITERATIONS}] {}s{}\x1b[0m",
                turn_started.elapsed().as_secs(),
                match running_jobs {
                    0 => String::new(),
                    n => format!(" · {n} job(s) running"),
                }
            );
            if iterations > MAX_TOOL_ITERATIONS {
                break Err("chat: exceeded maximum number of tool interactions".to_string());
            }

            // Checked before the request, not after: stopping once the bill is
            // already over the line would let a single expensive turn blow through
            // whatever number the user set.
            if let Some(budget) = setup.turn_token_budget
                && manager.turn_usage.total_tokens() >= budget
            {
                break Err(format!(
                    "chat: stopped after {} tokens, at the {TURN_TOKEN_BUDGET_KEY} of {budget}. \
                 Raise it, or ask a narrower question.",
                    manager.turn_usage.total_tokens()
                ));
            }

            // Told before any hook of this round can fire, so a governor hook
            // sees the round it is about to authorise rather than the last one.
            setup.hook_ctx.note_loop(loop_state(
                iterations,
                manager.turn_usage.prompt_tokens,
                manager.turn_usage.completion_tokens,
                setup.turn_token_budget,
            ));

            // Compact by rule before paying a model to summarize. Superseded and
            // stale tool output is most of what makes a long run large, and
            // dropping it costs nothing; on the runs where this is enough, the
            // summarization request below never happens.
            if manager.should_summarize() {
                let buffer_before = manager.buffer_size_chars();
                // Read before compaction, like `buffer_before`: the free pass
                // can bring the buffer back under its limit, and asking
                // afterwards then names `prompt_tokens` for a round the buffer
                // size triggered.
                let reason = if buffer_before > MAX_BUFFER_CHARS {
                    "buffer_chars"
                } else {
                    "prompt_tokens"
                };
                let reclaimed = manager.compact_buffer();
                if reclaimed > 0 {
                    tracing::debug!("compacted {reclaimed} chars of tool output out of the buffer");
                }

                // Fired after the free pass and before the paid one, so the
                // payload can say whether this round is about to cost anything.
                // Observation only: refusing compaction would leave the request
                // too large to send, so there is no safe `deny` to offer.
                let will_summarize = manager.should_summarize();
                setup.hook_ctx.fire(
                    hooks::HookEvent::PreCompact,
                    hooks::HookSubject::none(),
                    || {
                        json!({
                            "reason": reason,
                            "buffer_chars": manager.buffer_size_chars(),
                            "buffer_chars_before": buffer_before,
                            "buffer_messages": manager.buffer_len(),
                            "reclaimed_chars": reclaimed,
                            "last_prompt_tokens": manager.last_prompt_tokens(),
                            "prompt_token_budget": manager.prompt_token_budget(),
                            "will_summarize": will_summarize,
                        })
                    },
                    &|| proxy.is_canceled(),
                );
            }

            // Check for Summarization (may need multiple rounds if buffer is huge).
            // Bounded: a summary that fails to shrink the context must not turn into
            // an unbounded stream of paid requests.
            let mut summary_rounds = 0;
            while manager.should_summarize() && summary_rounds < MAX_SUMMARY_ROUNDS {
                summary_rounds += 1;
                // Graceful fallback on summary failure
                if let Err(e) = manager.perform_summary(client, proxy, model_override.clone()) {
                    if setup.runtime.is_some() {
                        break 'agent Err(e);
                    }
                    tracing::warn!("Context summarization failed: {e}, continuing without summary");
                    break; // Continue with current buffer, don't fail the whole conversation
                }
            }

            let mut current_messages =
                manager.build_messages_for_chat(dynamic_context.message(proxy));
            if let Some(runtime) = &setup.runtime {
                current_messages.push(json!({"role":"system","content":runtime.lock().context()}));
            }

            let options = ChatRequestOptions::new()
                .with_temperature(temperature)
                .with_model(model_override.clone())
                .with_tools(Some(tools.clone()))
                .with_prompt_cache_key(Some(PROMPT_CACHE_KEY.to_string()))
                .with_stream(stream_sink.is_some());

            let response = if let Some(sink) = stream_sink.as_deref_mut() {
                sink.begin_iteration();
                let spinner = SpinnerGuard::start("");
                let result = client.send_chat_streaming(
                    &current_messages,
                    &options,
                    &|| task_cancelled(proxy),
                    &mut |text| sink.on_delta(&spinner, text),
                );
                match result {
                    Ok(response) => {
                        sink.finish_iteration(&spinner);
                        response
                    }
                    Err(err) => {
                        // Whatever streamed before the failure (a dropped
                        // connection, a mid-stream error frame) is already
                        // generated - flush it instead of losing the tail end
                        // of a partial answer the user never gets to see.
                        sink.finish_iteration(&spinner);
                        break Err(if is_ctrl_c_cancelled(&err) {
                            err.to_string()
                        } else {
                            format!("chat: {err}")
                        });
                    }
                }
            } else {
                let _spinner = SpinnerGuard::start("");
                match client
                    .send_chat_cancellable(&current_messages, &options, &|| task_cancelled(proxy))
                {
                    Ok(response) => response,
                    Err(err) => {
                        break Err(if is_ctrl_c_cancelled(&err) {
                            err.to_string()
                        } else {
                            format!("chat: {err}")
                        });
                    }
                }
            };

            // Feed the measured prompt size back so the next summarization
            // decision is based on what the provider charged, not a byte proxy.
            manager.turn_usage.add_response(&response);
            // Updated a second time: the call above is what makes this round's
            // tokens known, and a `pre-tool-use` hook watching spend would
            // otherwise always be one round behind.
            setup.hook_ctx.note_loop(loop_state(
                iterations,
                manager.turn_usage.prompt_tokens,
                manager.turn_usage.completion_tokens,
                setup.turn_token_budget,
            ));
            if let Some(reported) = usage::TokenUsage::from_response(&response) {
                manager.note_prompt_tokens(reported.prompt_tokens);
            } else if setup.runtime.is_some() {
                break Err(
                    "agent: provider omitted token usage; cannot enforce the task budget".into(),
                );
            }

            let turn = match interpret_response(&response) {
                Ok(turn) => turn,
                Err(err) => break Err(format!("chat: {err}")),
            };

            // Streamed this round's text already appeared as rendered Markdown
            // blocks; a response that fell back to non-streaming (or streaming
            // is off) still owes the old dim interim-text line.
            let streamed_this_round = stream_sink
                .as_deref()
                .is_some_and(StreamSink::streamed_this_iteration);

            // A run of up to MAX_TOOL_ITERATIONS steps is otherwise a black box:
            // show the plan the model states alongside its tool calls.
            if !streamed_this_round && let Some(text) = &turn.interim_text {
                eprintln!("\x1b[2m{}\x1b[0m", text.trim());
            }

            if let Some(assistant_message) = turn.assistant_message {
                manager.add_message(assistant_message);
            }

            match turn.outcome {
                TurnOutcome::ToolCalls(tool_calls) => {
                    stalled_rounds = 0;
                    if setup.runtime.is_none() && verify_after_mutation && !verification_nudged {
                        mutating_calls += tool_calls
                            .iter()
                            .filter(|call| is_mutating_tool_call(call))
                            .count();
                    }
                    // `break Err(...)`, not `?`: every other failure in this
                    // loop (the streaming/response errors above, `Cut`,
                    // `Stalled`'s `GiveUp` below) reaches the loop's own
                    // `break` so the epilogue after it - `reflect`,
                    // `runtime.checkpoint`/`finish`, `report_turn_usage`,
                    // `rewind_to_turn_start`, `session::store` - always
                    // runs, per this function's own opening comment. A `?`
                    // here would be the one path that skips all of that:
                    // `before_tool`'s `bail!` (time budget exhausted, no plan
                    // recorded yet, ...) would propagate straight out of the
                    // closure, leaving `runtime.finish` never called and
                    // `dsh/src/agent.rs`'s own fallback recording a generic
                    // "chat exited: ExitedWith(1)" as `stop_reason` instead
                    // of the specific reason `agent/blocked.rs` needs to
                    // suggest the next command.
                    if let Err(err) = run_tool_calls(
                        &tool_calls,
                        mcp_manager,
                        setup.runtime.as_ref(),
                        &setup.hook_ctx,
                        proxy,
                        &mut manager,
                        &mut tools,
                    ) {
                        break Err(err);
                    }
                }
                TurnOutcome::Answer(content) => {
                    // Read the event log before the guard: `?` here would
                    // leave this closure before the epilogue's
                    // `runtime.finish` below, stranding the task `Running`.
                    let pending_remote = if let Some(runtime) = &setup.runtime {
                        let runtime = runtime.lock();
                        match runtime.store.events(&runtime.task.id) {
                            Ok(events) => crate::agent::pending_remote_tasks(&events),
                            Err(e) => break Err(e.to_string()),
                        }
                    } else {
                        false
                    };
                    if let Some(runtime) = &setup.runtime
                        && {
                            let runtime = runtime.lock();
                            !runtime.task.verified() || runtime.jobs.has_running() || pending_remote
                        }
                    {
                        unverified_answers += 1;
                        if unverified_answers >= 2 {
                            break Err("agent: cannot complete with unverified criteria".into());
                        }
                        manager.add_message(json!({"role":"user","content":"The task still has unverified criteria. Perform the checks and use task_verify with tool-result evidence, or explain the blocker. Do not claim completion."}));
                        continue;
                    }
                    if setup.runtime.is_none()
                        && verify_after_mutation
                        && !verification_nudged
                        && mutating_calls > 0
                    {
                        verification_nudged = true;
                        mutating_calls = 0;
                        manager.add_message(
                            json!({"role":"user","content": VERIFY_AFTER_MUTATION_NUDGE}),
                        );
                        continue;
                    }
                    break Ok(content);
                }
                TurnOutcome::Cut {
                    finish_reason,
                    partial,
                } => {
                    // Already on the screen for this round; showing it again in
                    // the error would duplicate it.
                    let partial = if streamed_this_round {
                        None
                    } else {
                        partial.as_deref()
                    };
                    break Err(format!(
                        "chat: {}",
                        turn::describe_cut(&finish_reason, partial)
                    ));
                }
                TurnOutcome::Stalled => {
                    // Nudge once, then stop instead of resending the same request
                    // until the iteration cap burns through the budget.
                    stalled_rounds += 1;
                    match turn::handle_stall(stalled_rounds) {
                        turn::StallAction::GiveUp(reason) => break Err(format!("chat: {reason}")),
                        turn::StallAction::Nudge(prompt) => {
                            manager.add_message(json!({ "role": "user", "content": prompt }));
                        }
                    }
                }
            }
        };

        // Before the checkpoint/finish below, so a task's final token total
        // includes whatever this spent, and before `manager` moves into
        // `session::store` further down. Never changes `outcome` - see
        // `reflect::maybe_reflect`'s own doc comment for why sending one
        // more request here is not a third agent loop.
        reflect::maybe_reflect(
            client,
            proxy,
            &mut manager,
            iterations,
            outcome.is_ok(),
            setup.turn_token_budget,
            model_override.clone(),
        );

        if let Some(runtime) = &setup.runtime {
            let mut runtime = runtime.lock();
            // Resilient, not `?`: a checkpoint failure must not skip `finish`
            // (which would strand the task `Running`) nor the rewind/store
            // and job cleanup below it.
            match serde_json::to_value(&manager) {
                Ok(snapshot) => {
                    if let Err(e) = runtime.checkpoint(snapshot, manager.turn_usage.total_tokens())
                    {
                        tracing::warn!("agent: turn-end checkpoint failed: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("agent: turn-end checkpoint serialization failed: {e}");
                }
            }
            if let Err(e) = runtime.finish(outcome.is_ok(), outcome.as_ref().err().cloned()) {
                tracing::warn!("agent: turn-end finish failed: {e}");
            }
        }
        report_turn_usage(&manager.turn_usage);
        // Read before `manager` is handed to the session store below.
        turn_tokens = (
            manager.turn_usage.prompt_tokens,
            manager.turn_usage.completion_tokens,
            manager.turn_usage.total_tokens(),
        );

        // A turn that fails is rewound to where it started, not discarded.
        // `take` already emptied the slot, so returning here without storing
        // anything used to throw away every earlier, successful turn too -
        // a single Ctrl-C, API error, iteration-cap or turn-token-budget stop
        // erased up to `AI_CHAT_SESSION_TTL_SECS` worth of context, not just
        // the turn that failed. `rewind_to_turn_start` only succeeds when this
        // turn continued a stored conversation (`turn_mark` is set); a brand
        // new conversation that fails still has nothing to keep.
        let rewound = outcome.is_err() && manager.rewind_to_turn_start();
        if rewound {
            // The rewound buffer is guaranteed balanced - see `turn_mark`'s
            // doc comment - so this cannot desync a `tool_calls`/`tool` pair.
            manager.note_turn_rewound();
        }
        // Only a turn that finished or was cleanly rewound is worth resuming.
        if outcome.is_ok() || rewound {
            session::store(
                setup.session_ttl,
                manager,
                setup.hook_ctx.session_id(),
                &setup.prompt.identity,
                // Cloned, not moved: `setup` is a shared `&TurnSetup` capture
                // (see `run_tool_calls`), and this closure runs only once.
                setup.scope.clone(),
                // A run of failures must not keep a dead conversation's idle
                // clock running: only a turn that actually finished restarts
                // it from now.
                if rewound { carried_stored_at } else { None },
            );
        }

        // A job may only outlive its turn when some later turn can still name
        // it. That means the conversation was actually stored - which needs
        // both a surviving outcome *and* a session TTL, because `store` is a
        // no-op without one. Checking the outcome alone left a job running with
        // nothing able to poll it whenever `AI_CHAT_SESSION_TTL_SECS=0`.
        if owns_chat_jobs {
            let carried_forward = setup.session_ttl.is_some() && (outcome.is_ok() || rewound);
            if carried_forward {
                // Kept, so say so: a process group nobody mentioned is one
                // nobody remembers to stop.
                for line in jobs::describe_running() {
                    eprintln!(
                        "\x1b[2mchat: job still running - {line}. chat_status to inspect, chat_reset to stop.\x1b[0m"
                    );
                }
            } else {
                let orphaned = jobs::cancel_session(setup.hook_ctx.session_id());
                if orphaned > 0 {
                    eprintln!(
                        "\x1b[2mchat: {orphaned} job(s) cancelled (this conversation is not carried forward)\x1b[0m"
                    );
                }
            }
        }

        outcome
    })();

    // One write per turn, not one per `read_file`: the loop can run a hundred
    // iterations and these are counters, not state anything depends on.
    skills::usage::flush();

    // Told after `flush()`, not before: this turn's own writes and reads are
    // already accounted for, so a sweep judges "unused" against numbers that
    // include what just happened rather than what stood before it.
    maybe_auto_archive_skills(proxy);

    let staged_this_turn =
        skills::pending::staged_this_process().saturating_sub(setup.staged_before_turn);
    if staged_this_turn > 0 {
        eprintln!(
            "\x1b[2mskills: {staged_this_turn} proposal(s) staged; review with `skill pending`\x1b[0m"
        );
    }

    // Observation only. "Keep going" is a request to spend more of the user's
    // money and touch more of their machine, which is the one thing a hook is
    // not allowed to ask for.
    // Keeps the envelope's `loop` in step with the `iterations` and `tokens`
    // this event has carried in its own detail since it was added.
    setup.hook_ctx.note_loop(loop_state(
        iterations,
        turn_tokens.0,
        turn_tokens.1,
        setup.turn_token_budget,
    ));
    setup.hook_ctx.fire(
        hooks::HookEvent::ResponseComplete,
        hooks::HookSubject::none(),
        || {
            json!({
                "status": if outcome.is_ok() { "ok" } else { "error" },
                "answer": outcome.as_ref().ok().map(|answer| hooks::redact(answer)),
                "error": outcome.as_ref().err().map(|err| hooks::redact(err)),
                "iterations": iterations,
                "tokens": {
                    "prompt": turn_tokens.0,
                    "completion": turn_tokens.1,
                    "total": turn_tokens.2,
                },
            })
        },
        // The turn is over; a cancelled turn still wants its final event.
        &hooks::never_cancelled,
    );

    outcome
}

#[cfg(test)]
mod tests;
