//! Prompt footprint profiler: where AI input tokens and prompt bytes go.
//!
//! Two fundamentally different measurements live here, and they must never be
//! confused:
//!
//! - **Deterministic local footprint**, in compact-JSON / UTF-8 bytes,
//!   measured offline from the exact values the runtime would send. This is
//!   not a token count, and no test may treat it as one.
//! - **Provider-reported token usage** (`prompt_tokens`,
//!   `cached_prompt_tokens`, `completion_tokens`) from a real completed model
//!   response. Only these may be called actual token counts.
//!
//! The fixed prompt (system prompt sections, interactive/agent tool surfaces,
//! MCP catalog) is measurable without credentials or network. The
//! conversation slice reflects the most recent main chat-loop request,
//! recorded in [`record_request`] just before sending and annotated with
//! provider usage in [`attach_provider_usage`] after a successful response.
//! Auxiliary requests (summarization, skill reflection) never touch the
//! snapshot. No prompt, tool-result, or secret content is stored here -
//! metrics only, so `doctor ai --prompt-size --json` is safe to capture in
//! CI logs.
use super::*;
use crate::shell_capabilities::AgentCommandPolicy;
use std::sync::{LazyLock, Mutex};

/// Diagnostic schema version for `doctor ai --prompt-size --json`.
///
/// Scripts may consume this output; future attribution changes must bump this
/// rather than silently reshaping fields. Unrelated to any re-exec protocol
/// version.
pub(crate) const FOOTPRINT_REPORT_VERSION: u32 = 2;

/// Which chat entry point a measured request belongs to: `!` interactive
/// turns and durable agent turns do not share a tool surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PromptSurface {
    Interactive,
    Agent,
}

/// Section byte sizes of the rendered system prompt, measured by staged
/// assembly deltas. The parts always sum to `total_text_bytes`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SystemPromptFootprint {
    pub total_text_bytes: usize,
    pub base_guidance_bytes: usize,
    pub skills_bytes: usize,
    pub mcp_guidance_bytes: usize,
    pub operator_bytes: usize,
    pub language_bytes: usize,
}

/// One tool definition's share of a tool surface: name plus exact compact
/// JSON bytes. Never ranked by description length.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ToolDefinitionFootprint {
    pub name: String,
    pub json_bytes: usize,
}

/// The exact `tools` array one entry point would send, largest definition
/// first.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ToolSurfaceFootprint {
    pub count: usize,
    /// Compact serialized bytes of the whole array, framing included.
    pub tools_json_bytes: usize,
    /// Sum of the individual definition bytes, without array framing.
    pub sum_definition_bytes: usize,
    /// `tools_json_bytes - sum_definition_bytes`: `[`, `]`, and commas.
    /// Exposed so the two figures above add up exactly.
    pub array_overhead_bytes: usize,
    pub tools: Vec<ToolDefinitionFootprint>,
}

/// MCP attribution: what exists versus what the model actually sees.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct McpSurfaceFootprint {
    pub discoverable_tools: usize,
    pub discoverable_direct_schema_bytes: usize,
    pub active_tools: usize,
    pub active_direct_schema_bytes: usize,
    /// Definitions on the selected model surface (bridge trio in bridge
    /// mode, meta tools plus active schemas in eager mode).
    pub model_surface_tools: usize,
    pub model_surface_bytes: usize,
}

/// Everything fixed before a turn starts, measurable offline.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct FixedPromptFootprint {
    pub mcp_mode: McpToolMode,
    pub system_prompt: SystemPromptFootprint,
    pub interactive_tools: ToolSurfaceFootprint,
    pub agent_tools: ToolSurfaceFootprint,
    pub mcp_catalog: McpSurfaceFootprint,
    /// Project skill roots that hold skills but are not trusted yet. Their
    /// content is deliberately excluded from `system_prompt.skills_bytes`:
    /// the profiler reports what would be sent right now.
    pub project_skill_roots_pending_trust: usize,
}

/// Category byte sizes of the conversation behind one request.
///
/// `assistant_tool_call_json_bytes` is a sub-part of `assistant_json_bytes`
/// (the serialized `tool_calls` arrays), not an additional addend: the two
/// overlap by construction. Every other category is disjoint, and
/// `total_object_bytes` is exactly their sum.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct ConversationFootprint {
    pub pinned_system_json_bytes: usize,
    pub pinned_goal_json_bytes: usize,
    pub summary_json_bytes: usize,
    pub summary_messages: usize,
    pub user_json_bytes: usize,
    pub user_messages: usize,
    pub assistant_json_bytes: usize,
    pub assistant_tool_call_json_bytes: usize,
    pub assistant_messages: usize,
    pub tool_result_json_bytes: usize,
    pub tool_messages: usize,
    pub system_notice_json_bytes: usize,
    pub system_messages: usize,
    pub other_json_bytes: usize,
    pub other_messages: usize,
    pub total_object_bytes: usize,
    pub total_messages: usize,
}

/// Provider-reported usage for one request. `cache_hit_ratio` is `None` when
/// the provider reported no prompt tokens; `cache_reporting_available` is
/// false when the provider omitted the cached-tokens field entirely, which
/// must render as "unavailable", never as `0%`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProviderUsageFootprint {
    pub prompt_tokens: u64,
    pub cached_prompt_tokens: u64,
    pub uncached_prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_ratio: Option<f64>,
    pub cache_reporting_available: bool,
}

/// One main chat-loop request: exact local payload sizes plus, after a
/// successful response, the provider's own token accounting.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RequestFootprint {
    pub surface: PromptSurface,
    pub iteration: usize,
    pub mcp_mode: McpToolMode,
    pub messages_json_bytes: usize,
    pub sum_message_object_bytes: usize,
    pub messages_array_overhead_bytes: usize,
    pub tools_json_bytes: usize,
    pub sum_tool_definition_bytes: usize,
    pub tools_array_overhead_bytes: usize,
    /// Local payload proxy: `messages_json_bytes + tools_json_bytes`.
    /// Never the provider's tokenizer input length.
    pub context_json_bytes: usize,
    pub conversation: ConversationFootprint,
    pub observations: ObservationFootprint,
    pub dynamic_context_json_bytes: usize,
    pub agent_runtime_context_json_bytes: usize,
    pub provider_usage: Option<ProviderUsageFootprint>,
}

/// Observation Store attribution: lifetime storage versus active references.
///
/// `stored_*` is everything retained for the conversation lifetime;
/// `active_*` counts only stubs currently in the provider-bound
/// conversation. Only active references contribute current prompt saving,
/// measured as exact deterministic `original serialized tool-message bytes
/// minus current stub-message bytes` (`active_reclaimed_json_bytes`). Never
/// a token count.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct ObservationFootprint {
    pub stored_entries: usize,
    pub stored_content_bytes: usize,
    pub active_references: usize,
    pub active_original_message_bytes: usize,
    pub active_stub_message_bytes: usize,
    pub active_reclaimed_json_bytes: usize,
}

/// The full profiler report: fixed offline measurements plus the last real
/// main-loop request, if any turn has run in this process.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PromptFootprintReport {
    pub version: u32,
    pub fixed: FixedPromptFootprint,
    pub last_request: Option<RequestFootprint>,
}

/// Compact serialized JSON bytes of one value: the deterministic local
/// metric. UTF-8 bytes, not tokens. Shared with `conversation.rs` so the
/// per-category byte sizes use the same ruler as the request totals here.
pub(super) fn compact_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|body| body.len())
        .unwrap_or(0)
}

/// Compact serialized bytes of a JSON array: framing (`[`, `]`, commas)
/// included, unlike the sum of its elements.
fn compact_array_len(values: &[Value]) -> usize {
    serde_json::to_vec(values)
        .map(|body| body.len())
        .unwrap_or(0)
}

fn tool_definition_name(definition: &Value) -> String {
    definition
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// Measure the exact `tools` array that would be sent, definition by
/// definition, largest first.
pub(super) fn tool_surface_footprint(tools: &[Value]) -> ToolSurfaceFootprint {
    let mut definitions: Vec<ToolDefinitionFootprint> = tools
        .iter()
        .map(|definition| ToolDefinitionFootprint {
            name: tool_definition_name(definition),
            json_bytes: compact_len(definition),
        })
        .collect();
    // Largest first for the human report; names never truncated.
    definitions.sort_by(|left, right| {
        right
            .json_bytes
            .cmp(&left.json_bytes)
            .then_with(|| left.name.cmp(&right.name))
    });
    let sum_definition_bytes = definitions.iter().map(|item| item.json_bytes).sum();
    let tools_json_bytes = compact_array_len(tools);
    ToolSurfaceFootprint {
        count: tools.len(),
        tools_json_bytes,
        sum_definition_bytes,
        array_overhead_bytes: tools_json_bytes.saturating_sub(sum_definition_bytes),
        tools: definitions,
    }
}

/// Pure measurement of one request: array bytes, per-object sums with their
/// framing overhead made explicit, conversation categories, and the volatile
/// context slices. Recording and provider annotation happen separately in
/// [`record_request`] and [`attach_provider_usage`].
#[allow(clippy::too_many_arguments)]
pub(super) fn snapshot_request(
    surface: PromptSurface,
    iteration: usize,
    mcp_mode: McpToolMode,
    messages: &[Value],
    tools: &[Value],
    manager: &ConversationManager,
    dynamic_context: &Value,
    runtime_context: Option<&Value>,
) -> RequestFootprint {
    let sum_message_object_bytes = messages.iter().map(compact_len).sum();
    let messages_json_bytes = compact_array_len(messages);
    let sum_tool_definition_bytes = tools.iter().map(compact_len).sum();
    let tools_json_bytes = compact_array_len(tools);
    let dynamic_context_json_bytes = compact_len(dynamic_context);
    let agent_runtime_context_json_bytes = runtime_context.map(compact_len).unwrap_or(0);
    RequestFootprint {
        surface,
        iteration,
        mcp_mode,
        messages_json_bytes,
        sum_message_object_bytes,
        messages_array_overhead_bytes: messages_json_bytes.saturating_sub(sum_message_object_bytes),
        tools_json_bytes,
        sum_tool_definition_bytes,
        tools_array_overhead_bytes: tools_json_bytes.saturating_sub(sum_tool_definition_bytes),
        context_json_bytes: messages_json_bytes.saturating_add(tools_json_bytes),
        conversation: manager.footprint(),
        observations: manager.observation_footprint(),
        dynamic_context_json_bytes,
        agent_runtime_context_json_bytes,
        provider_usage: None,
    }
}

/// Assemble this iteration's `messages` array exactly the way the loop
/// sends it - stable conversation first, volatile environment snapshot
/// last, durable agent context after that - and record its local footprint
/// before sending.
///
/// Returns the assembled messages. Summary and reflection requests never
/// pass through here, so they cannot overwrite the main-request snapshot.
pub(super) fn build_and_record_request(
    setup: &TurnSetup,
    manager: &ConversationManager,
    dynamic_context: &mut DynamicContext,
    request_tools: &[Value],
    iteration: usize,
    proxy: &mut dyn ChatToolHost,
) -> Vec<Value> {
    let dynamic_message = dynamic_context.message(proxy);
    let mut current_messages = manager.build_messages_for_chat(dynamic_message.clone());
    let runtime_message: Option<Value> = setup
        .runtime
        .as_ref()
        .map(|runtime| json!({"role":"system","content":runtime.lock().context()}));
    if let Some(note) = &runtime_message {
        current_messages.push(note.clone());
    }
    // Recorded before sending, with provider usage still empty: the
    // attempted size stays known even when the request itself fails.
    record_request(snapshot_request(
        if setup.runtime.is_some() {
            PromptSurface::Agent
        } else {
            PromptSurface::Interactive
        },
        iteration,
        setup.mcp_tool_mode,
        &current_messages,
        request_tools,
        manager,
        &dynamic_message,
        runtime_message.as_ref(),
    ));
    current_messages
}

/// The last main chat-loop request's footprint: metrics only, never prompt
/// text, tool results, or file contents.
static LAST_REQUEST_FOOTPRINT: LazyLock<Mutex<Option<RequestFootprint>>> =
    LazyLock::new(|| Mutex::new(None));

/// Serialize test access to the process-wide snapshot.
///
/// The test harness runs tests in parallel threads; without this, a
/// `record_request` from one test can land between another test's seed and
/// its assertion and flake it. Production needs no equivalent: chat turns
/// are synchronous and single-threaded per process.
#[cfg(test)]
static LAST_REQUEST_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Hold across a test's whole seed-act-assert sequence on the snapshot.
/// Follows the existing `env_lock()` pattern for shared global test state.
#[cfg(test)]
pub(crate) fn snapshot_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LAST_REQUEST_TEST_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

/// Record a main-loop request immediately before sending, with
/// `provider_usage` still `None`, so the attempted size survives network
/// failures, provider 500s, and stream disconnects.
///
/// `pub(crate)` rather than `pub(super)` so `doctor` tests can seed a
/// deterministic snapshot without running a chat turn.
pub(crate) fn record_request(footprint: RequestFootprint) {
    if let Ok(mut slot) = LAST_REQUEST_FOOTPRINT.lock() {
        *slot = Some(footprint);
    }
}

/// Annotate the recorded request with the provider's own usage block.
///
/// Called only for main-loop responses: summarization and reflection
/// requests never pass through here, so their usage cannot overwrite the
/// main request's snapshot.
pub(super) fn attach_provider_usage(response: &Value) {
    let Some(usage) = usage::TokenUsage::from_response(response) else {
        return;
    };
    let footprint = ProviderUsageFootprint {
        prompt_tokens: usage.prompt_tokens,
        cached_prompt_tokens: usage.cached_prompt_tokens,
        uncached_prompt_tokens: usage.uncached_prompt_tokens(),
        completion_tokens: usage.completion_tokens,
        cache_hit_ratio: usage.cache_hit_ratio(),
        cache_reporting_available: response_has_cached_tokens(response),
    };
    if let Ok(mut slot) = LAST_REQUEST_FOOTPRINT.lock()
        && let Some(recorded) = slot.as_mut()
    {
        recorded.provider_usage = Some(footprint);
    }
}

/// Did the provider actually report cached tokens, as opposed to omitting
/// the field (or reporting an explicit null)? `TokenUsage` normalizes all
/// three to `0`, so this inspects the raw response: a missing field renders
/// as "unavailable", never `0%`.
fn response_has_cached_tokens(response: &Value) -> bool {
    let Some(usage) = response.get("usage") else {
        return false;
    };
    if usage
        .get("cached_tokens")
        .is_some_and(|value| !value.is_null())
    {
        return true;
    }
    usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .is_some_and(|value| !value.is_null())
}

/// The recorded footprint of the last main chat-loop request, if this
/// process has sent one.
pub(crate) fn last_request() -> Option<RequestFootprint> {
    LAST_REQUEST_FOOTPRINT
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
}

/// Resolve this turn's [`McpToolMode`] from the operator preference.
///
/// The one resolver diagnostics use, so `doctor` and the chat runtime cannot
/// disagree about which surface a measurement describes.
pub(crate) fn resolve_mcp_mode(proxy: &mut dyn ShellProxy, manager: &McpManager) -> McpToolMode {
    manager.resolve_mcp_tool_mode(
        resolve_mcp_tool_mode_preference(proxy),
        DEFAULT_MCP_BRIDGE_SCHEMA_BYTES,
    )
}

/// Mode-aware MCP status for `doctor mcp`: what exists, what the model sees,
/// and whether eager mode is carrying enough schema bytes to warn about.
pub(crate) struct McpFootprintStatus {
    pub mode: McpToolMode,
    pub discoverable_tools: usize,
    pub discoverable_schema_bytes: usize,
    pub active_tools: usize,
    pub active_schema_bytes: usize,
    /// Eager mode carrying more than the auto-bridge threshold of active
    /// schema bytes. Bridge mode never warns: its surface is bounded by
    /// construction.
    pub warn_over_threshold: bool,
}

/// Resolve the mode and measure the MCP status in one pass, for
/// diagnostics that need both: the discoverable footprint the decision was
/// made on is reused instead of serializing the catalog twice.
pub(crate) fn mcp_footprint_status_from(
    proxy: &mut dyn ShellProxy,
    manager: &McpManager,
) -> McpFootprintStatus {
    let (mode, discoverable) = manager.resolve_mcp_tool_mode_and_footprint(
        resolve_mcp_tool_mode_preference(proxy),
        DEFAULT_MCP_BRIDGE_SCHEMA_BYTES,
    );
    let exposure = manager.tool_exposure();
    McpFootprintStatus {
        mode,
        discoverable_tools: discoverable.tools,
        discoverable_schema_bytes: discoverable.schema_bytes,
        active_tools: exposure.active_tools,
        active_schema_bytes: exposure.schema_bytes,
        warn_over_threshold: mode == McpToolMode::Eager
            && exposure.schema_bytes > DEFAULT_MCP_BRIDGE_SCHEMA_BYTES,
    }
}

/// Measure the fixed prompt offline: no API key, no network, no prompts.
///
/// Resolves skill roots, operator prompt, language, and MCP mode through the
/// same canonical resolvers a real turn uses, but only consults the already-
/// trusted project-skill state - untrusted roots are counted in
/// `project_skill_roots_pending_trust` and excluded from the measured
/// prompt. Never calls MCP `tools/list`: the currently loaded catalog is the
/// measurement basis.
///
/// Infallible by construction: every input resolves to a default rather
/// than an error, so there is no unreachable `Err` arm for callers to
/// maintain.
pub(crate) fn fixed_prompt_footprint(proxy: &mut dyn ShellProxy) -> FixedPromptFootprint {
    let cwd = proxy.get_current_dir().ok();
    let mut skill_roots =
        skills::skill_roots(cwd.as_deref(), resolve_project_skills_enabled(proxy));

    let decisions = skills::describe_project_roots(&skill_roots);
    let mut untrusted: Vec<std::path::PathBuf> = Vec::new();
    for decision in &decisions {
        if !project_skill_root_already_trusted(decision, &mut *proxy as &mut dyn AgentCommandPolicy)
        {
            untrusted.push(decision.root.clone());
        }
    }
    let project_skill_roots_pending_trust = untrusted.len();
    skill_roots.retain(|root| !untrusted.contains(&root.path));

    let mcp_handle = proxy.agent_mcp_manager();
    let mcp = mcp_handle.read();
    // Single pass: the discoverable footprint below is the one the mode
    // decision was made on, not a second serialization of the catalog.
    let (mcp_mode, discoverable) = mcp.resolve_mcp_tool_mode_and_footprint(
        resolve_mcp_tool_mode_preference(proxy),
        DEFAULT_MCP_BRIDGE_SCHEMA_BYTES,
    );

    let prompt = build_system_prompt(
        proxy.get_var(PROMPT_KEY).as_deref().map(str::to_string),
        response_language(proxy),
        &mcp,
        &skill_roots,
        mcp_mode,
    );

    // The same tool arrays a real turn would send, with an empty
    // ToolSearchExposure: turn-local search hits are per-turn state, not
    // fixed prompt.
    let empty_exposure = ToolSearchExposure::default();
    let (interactive_base, _) = split_turn_tool_bases(&mcp_handle, false, mcp_mode);
    let interactive_tools_vec = build_request_tools(
        mcp_mode,
        &interactive_base,
        &[],
        &mcp_handle,
        &empty_exposure,
    );
    let (_, agent_accumulated) = split_turn_tool_bases(&mcp_handle, true, mcp_mode);

    // The MCP slice of the interactive model surface: the bridge trio in
    // bridge mode, meta tools plus active schemas in eager mode.
    let model_surface_vec: Vec<Value> = match mcp_mode {
        McpToolMode::Bridge => {
            if mcp.is_empty() {
                Vec::new()
            } else {
                tool::mcp_bridge::bridge_definitions()
            }
        }
        McpToolMode::Eager => tool::mcp_turn_definitions(&mcp, true),
    };
    let exposure = mcp.tool_exposure();

    FixedPromptFootprint {
        mcp_mode,
        system_prompt: prompt.footprint,
        interactive_tools: tool_surface_footprint(&interactive_tools_vec),
        agent_tools: tool_surface_footprint(&agent_accumulated),
        mcp_catalog: McpSurfaceFootprint {
            discoverable_tools: discoverable.tools,
            discoverable_direct_schema_bytes: discoverable.schema_bytes,
            active_tools: exposure.active_tools,
            active_direct_schema_bytes: exposure.schema_bytes,
            model_surface_tools: model_surface_vec.len(),
            model_surface_bytes: compact_array_len(&model_surface_vec),
        },
        project_skill_roots_pending_trust,
    }
}

#[cfg(test)]
mod prompt_footprint_tests;
