//! Regression tests for the prompt footprint profiler.
//!
//! Every assertion here is about measurement, never about optimization:
//! local sizes are compact-JSON / UTF-8 bytes (not tokens), and only
//! provider-reported usage may be called token counts. Reductions pass
//! automatically (`actual <= maximum`); intentional growth requires an
//! explicit baseline update in `budget.json`, visible in review.
use super::*;
use std::collections::{HashMap, VecDeque};

fn empty_mcp_handle() -> Arc<RwLock<McpManager>> {
    Arc::new(RwLock::new(McpManager::default()))
}

fn assistant_call(id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "role": "assistant",
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": arguments }
        }]
    })
}

fn tool_reply(id: &str, content: &str) -> Value {
    json!({ "role": "tool", "tool_call_id": id, "content": content })
}

fn manager_with(buffer: Vec<Value>) -> ConversationManager {
    let mut manager = ConversationManager::new(
        json!({ "role": "system", "content": "sys" }),
        json!({ "role": "user", "content": "goal" }),
    );
    for message in buffer {
        manager.add_message(message);
    }
    manager
}

/// A [`ChatClient`] returning canned responses in order, so the live
/// request-recording path (`record_request` / `attach_provider_usage`) can
/// be driven without any network provider.
struct ScriptedClient {
    responses: Mutex<VecDeque<Value>>,
}

impl ScriptedClient {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

impl ChatClient for ScriptedClient {
    fn send_chat_request(
        &self,
        _messages: &[Value],
        _options: &ChatRequestOptions,
    ) -> anyhow::Result<Value> {
        self.responses
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front())
            .ok_or_else(|| anyhow::anyhow!("ScriptedClient: no more scripted responses"))
    }
}

fn final_answer(content: &str) -> Value {
    json!({
        "choices": [{
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": content },
        }],
    })
}

fn hermetic_proxy(cwd: std::path::PathBuf) -> crate::test_support::TestShellProxy {
    crate::test_support::TestShellProxy {
        current_dir: cwd,
        vars: HashMap::from([
            (
                hooks::config::HOOKS_ENABLED_KEY.to_string(),
                "off".to_string(),
            ),
            (session::SESSION_TTL_KEY.to_string(), "0".to_string()),
        ]),
        ..crate::test_support::TestShellProxy::default()
    }
}

fn mcp_manager_with_tools(servers: usize, tools_per_server: usize) -> McpManager {
    let mut manager = McpManager::default();
    for server in 0..servers {
        let label = format!("bulk{server:02}");
        for tool in 0..tools_per_server {
            manager.insert_test_tool(&label, &format!("helper_{tool:03}"));
        }
    }
    manager
}

/// Every section present: the parts must sum to the total, and the total
/// must be the rendered prompt's own byte length.
#[test]
fn prompt_footprint_system_sections_add_up() {
    let mut mcp = McpManager::default();
    mcp.insert_test_tool("github", "list_issues");
    let (text, footprint) = assemble_system_prompt_traced(
        "\n\n## Agent Skills\n- `demo`: does things\n",
        Some("Be polite."),
        Some("Japanese"),
        &mcp,
        McpToolMode::Eager,
    );

    assert!(footprint.base_guidance_bytes > 0);
    assert!(footprint.skills_bytes > 0);
    assert!(footprint.mcp_guidance_bytes > 0);
    assert!(footprint.operator_bytes > 0);
    assert!(footprint.language_bytes > 0);
    assert_eq!(
        footprint.base_guidance_bytes
            + footprint.skills_bytes
            + footprint.mcp_guidance_bytes
            + footprint.operator_bytes
            + footprint.language_bytes,
        footprint.total_text_bytes,
    );
    assert_eq!(footprint.total_text_bytes, text.len());
}

/// Multibyte text is measured in UTF-8 bytes, never `.chars().count()`.
#[test]
fn prompt_footprint_unicode_sections_add_up() {
    let operator = "丁寧に日本語で答えてください。";
    let skills = "\n\n## Agent Skills\n- `要約`: 日本語の説明文です\n";
    let (text, footprint) = assemble_system_prompt_traced(
        skills,
        Some(operator),
        Some("日本語"),
        &McpManager::default(),
        McpToolMode::Eager,
    );

    assert!(text.len() > text.chars().count());
    assert_eq!(
        footprint.base_guidance_bytes
            + footprint.skills_bytes
            + footprint.mcp_guidance_bytes
            + footprint.operator_bytes
            + footprint.language_bytes,
        footprint.total_text_bytes,
    );
    assert_eq!(footprint.total_text_bytes, text.len());
    assert_eq!(footprint.mcp_guidance_bytes, 0);
    assert!(footprint.operator_bytes >= operator.len());
}

/// The empty assembly is exactly the base guidance: the refactor changed no
/// rendered byte.
#[test]
fn prompt_footprint_empty_sections_render_only_base_guidance() {
    let (text, footprint) =
        assemble_system_prompt_traced("", None, None, &McpManager::default(), McpToolMode::Eager);

    assert_eq!(text, TOOL_SYSTEM_PROMPT);
    assert_eq!(footprint.base_guidance_bytes, TOOL_SYSTEM_PROMPT.len());
    assert_eq!(footprint.skills_bytes, 0);
    assert_eq!(footprint.mcp_guidance_bytes, 0);
    assert_eq!(footprint.operator_bytes, 0);
    assert_eq!(footprint.language_bytes, 0);
    assert_eq!(footprint.total_text_bytes, text.len());
}

/// `apply_language` appends (or no-ops); the language delta assumes a
/// prefix, and this test fails loudly if that ever stops being true.
#[test]
fn prompt_footprint_language_stage_is_append_only() {
    assert_eq!(dsh_openai::apply_language("base", None), "base");
    let rendered = dsh_openai::apply_language("base", Some("日本語"));
    assert!(rendered.starts_with("base"));

    let (_, footprint) = assemble_system_prompt_traced(
        "",
        None,
        Some("Japanese"),
        &McpManager::default(),
        McpToolMode::Eager,
    );
    assert!(footprint.language_bytes > 0);

    let (_, plain) =
        assemble_system_prompt_traced("", None, None, &McpManager::default(), McpToolMode::Eager);
    assert_eq!(plain.language_bytes, 0);
}

/// The instrumented assembly renders byte-for-byte what the old contract
/// expects, and the identity still excludes the skills list.
#[test]
fn prompt_footprint_rendering_matches_the_legacy_contract() {
    let mut mcp = McpManager::default();
    mcp.insert_test_tool("github", "list_issues");
    let skills = "\n\n## Agent Skills\n- `demo`: does things\n";

    let (traced, _) =
        assemble_system_prompt_traced(skills, Some("Be polite."), None, &mcp, McpToolMode::Eager);
    let legacy = assemble_system_prompt(skills, Some("Be polite."), None, &mcp, McpToolMode::Eager);
    assert_eq!(traced, legacy);
    assert!(traced.contains("\n\nMCP access:\n"));
    assert!(traced.contains("\n\nAdditional operator instructions:\nBe polite."));

    let prompt = build_system_prompt(
        Some("Be polite.".to_string()),
        None,
        &mcp,
        &[],
        McpToolMode::Eager,
    );
    assert_eq!(prompt.footprint.total_text_bytes, prompt.text.len());
    assert!(
        prompt.footprint.base_guidance_bytes
            + prompt.footprint.skills_bytes
            + prompt.footprint.mcp_guidance_bytes
            + prompt.footprint.operator_bytes
            + prompt.footprint.language_bytes
            == prompt.footprint.total_text_bytes
    );
}

/// Tool surfaces are measured as compact JSON: array bytes equal the summed
/// definitions plus explicit framing, definitions sort largest first, and
/// names are never truncated.
#[test]
fn prompt_footprint_tool_surface_accounting() {
    let tools = tool::build_tools();
    let surface = tool_surface_footprint(&tools);

    assert_eq!(surface.count, tools.len());
    assert_eq!(
        surface.tools_json_bytes,
        serde_json::to_vec(&tools).unwrap().len()
    );
    assert_eq!(
        surface.sum_definition_bytes + surface.array_overhead_bytes,
        surface.tools_json_bytes
    );
    assert!(surface.array_overhead_bytes >= 2);
    let mut sizes: Vec<usize> = surface.tools.iter().map(|item| item.json_bytes).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable_by(|left, right| right.cmp(left));
    assert_eq!(sizes, sorted);
    for item in &surface.tools {
        assert!(!item.name.is_empty());
        assert!(item.json_bytes > 0);
    }
    // Every definition is accounted for exactly once.
    assert_eq!(
        surface.sum_definition_bytes,
        tools
            .iter()
            .map(|definition| serde_json::to_vec(definition).unwrap().len())
            .sum::<usize>()
    );
    let _ = sizes.pop();
}

/// Bridge mode keeps the request tool payload constant while the catalog
/// grows 10x and 50x; only the discoverable catalog side may move.
#[test]
fn prompt_footprint_bridge_surface_is_catalog_independent() {
    let base = Some(tool::build_tools());
    let accumulated: Vec<Value> = Vec::new();
    let exposure = ToolSearchExposure::default();
    let mut payloads = Vec::new();
    let mut discoverable = Vec::new();

    for total in [10usize, 100, 500] {
        let servers = total.div_ceil(25);
        let per_server = total.div_ceil(servers);
        let handle = Arc::new(RwLock::new(mcp_manager_with_tools(servers, per_server)));
        let request =
            build_request_tools(McpToolMode::Bridge, &base, &accumulated, &handle, &exposure);
        payloads.push(serde_json::to_vec(&request).unwrap().len());
        let names: Vec<&str> = request
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        for name in ["tool_search", "tool_describe", "tool_call"] {
            assert!(names.contains(&name), "{names:?}");
        }
        assert!(
            !names.iter().any(|name| name.starts_with("mcp__")),
            "{names:?}"
        );
        discoverable.push(handle.read().discoverable_tool_footprint().schema_bytes);
    }

    assert_eq!(payloads[0], payloads[1]);
    assert_eq!(payloads[1], payloads[2]);
    assert!(
        discoverable[0] < discoverable[1] && discoverable[1] < discoverable[2],
        "the catalog side must still see the growth: {discoverable:?}"
    );
}

/// Eager mode grows with the catalog: the profiler can tell catalog bytes
/// from selected model-surface bytes.
#[test]
fn prompt_footprint_eager_surface_grows_with_the_catalog() {
    let base = Some(tool::build_tools());
    let accumulated: Vec<Value> = Vec::new();
    let exposure = ToolSearchExposure::default();
    let mut payloads = Vec::new();

    for total in [10usize, 100, 500] {
        let servers = total.div_ceil(25);
        let per_server = total.div_ceil(servers);
        let handle = Arc::new(RwLock::new(mcp_manager_with_tools(servers, per_server)));
        let request =
            build_request_tools(McpToolMode::Eager, &base, &accumulated, &handle, &exposure);
        payloads.push(serde_json::to_vec(&request).unwrap().len());
    }

    assert!(
        payloads[0] < payloads[1] && payloads[1] < payloads[2],
        "eager payloads must grow: {payloads:?}"
    );
}

/// One request snapshot: arrays equal summed objects plus framing, the
/// context is messages plus tools, and every conversation category is
/// populated and internally consistent.
#[test]
fn prompt_footprint_snapshot_categories_add_up() {
    let mut manager = manager_with(vec![
        json!({ "role": "user", "content": "first question" }),
        assistant_call("a", "read_file", r#"{"path":"src/main.rs"}"#),
        tool_reply("a", &"x".repeat(3000)),
        json!({ "role": "user", "content": "follow-up" }),
        json!({ "role": "assistant", "content": "done" }),
        json!({ "role": "system", "content": "a notice" }),
    ]);
    manager.summary = Some("earlier work".to_string());
    let dynamic = json!({ "role": "user", "content": "Environment snapshot: here" });
    let runtime = json!({ "role": "system", "content": "task state" });

    let mut messages = manager.build_messages_for_chat(dynamic.clone());
    messages.push(runtime.clone());
    let tools = tool::build_tools();
    let snapshot = snapshot_request(
        PromptSurface::Interactive,
        1,
        McpToolMode::Eager,
        &messages,
        &tools,
        &manager,
        &dynamic,
        Some(&runtime),
    );

    assert!(snapshot.messages_json_bytes > snapshot.sum_message_object_bytes);
    assert!(snapshot.messages_array_overhead_bytes >= 2);
    assert_eq!(
        snapshot.sum_message_object_bytes + snapshot.messages_array_overhead_bytes,
        snapshot.messages_json_bytes
    );
    assert_eq!(
        snapshot.sum_tool_definition_bytes + snapshot.tools_array_overhead_bytes,
        snapshot.tools_json_bytes
    );
    assert_eq!(
        snapshot.context_json_bytes,
        snapshot.messages_json_bytes + snapshot.tools_json_bytes
    );
    // The messages array is exactly the categorized conversation plus the
    // two volatile slices: nothing unaccounted for.
    assert_eq!(
        snapshot.sum_message_object_bytes,
        snapshot.conversation.total_object_bytes
            + snapshot.dynamic_context_json_bytes
            + snapshot.agent_runtime_context_json_bytes
    );

    let conversation = &snapshot.conversation;
    assert!(conversation.pinned_system_json_bytes > 0);
    assert!(conversation.pinned_goal_json_bytes > 0);
    assert!(conversation.summary_json_bytes > 0);
    assert_eq!(conversation.summary_messages, 1);
    assert!(conversation.user_json_bytes > 0);
    assert!(conversation.user_messages >= 2);
    assert!(conversation.assistant_json_bytes > 0);
    assert!(conversation.assistant_tool_call_json_bytes > 0);
    assert!(conversation.tool_result_json_bytes > 0);
    assert_eq!(conversation.tool_messages, 1);
    assert!(conversation.system_notice_json_bytes > 0);
    assert_eq!(
        conversation.total_object_bytes,
        conversation.pinned_system_json_bytes
            + conversation.pinned_goal_json_bytes
            + conversation.summary_json_bytes
            + conversation.user_json_bytes
            + conversation.assistant_json_bytes
            + conversation.tool_result_json_bytes
            + conversation.system_notice_json_bytes
            + conversation.other_json_bytes
    );
    assert_eq!(snapshot.dynamic_context_json_bytes, compact_len(&dynamic));
    assert_eq!(
        snapshot.agent_runtime_context_json_bytes,
        compact_len(&runtime)
    );
    assert!(snapshot.provider_usage.is_none());
}

/// Compaction observably shrinks the tool-result slice without dropping any
/// message.
#[test]
fn prompt_footprint_compaction_shrinks_tool_results() {
    let mut buffer = Vec::new();
    for index in 0..8 {
        let id = format!("c{index}");
        buffer.push(assistant_call(
            &id,
            "search",
            &format!(r#"{{"query":"q{index}"}}"#),
        ));
        buffer.push(tool_reply(&id, &"y".repeat(2000)));
    }
    let mut manager = manager_with(buffer);
    let before = manager.footprint().tool_result_json_bytes;
    assert!(before > 0);

    manager.compact_buffer();
    let after = manager.footprint();

    assert!(after.tool_result_json_bytes < before);
    assert_eq!(after.tool_messages, 8);
    assert_eq!(after.total_messages, manager.buffer.len() + 2);
}

/// A real turn through `chat_with_tools` records the request footprint and
/// attaches the provider's usage: 1200 prompt, 900 cached, 300 uncached.
#[test]
fn prompt_footprint_live_request_records_provider_usage() {
    let _snapshot = snapshot_test_guard();
    let cwd = tempfile::tempdir().unwrap();
    let mut proxy = hermetic_proxy(cwd.path().to_path_buf());
    let mut response = final_answer("done");
    response["usage"] = json!({
        "prompt_tokens": 1200,
        "completion_tokens": 50,
        "prompt_tokens_details": { "cached_tokens": 900 },
    });
    let client = ScriptedClient::new(vec![response]);
    let mcp_manager = empty_mcp_handle();

    let result = chat_with_tools(
        &client,
        "hello",
        None,
        None,
        Some(0.0),
        None,
        &mcp_manager,
        None,
        &mut proxy,
    );
    assert_eq!(result, Ok("done".to_string()));

    let recorded = last_request().expect("a main request ran");
    assert_eq!(recorded.surface, PromptSurface::Interactive);
    assert!(recorded.messages_json_bytes > 0);
    assert!(recorded.tools_json_bytes > 0);
    let usage = recorded.provider_usage.expect("provider usage attached");
    assert_eq!(usage.prompt_tokens, 1200);
    assert_eq!(usage.cached_prompt_tokens, 900);
    assert_eq!(usage.uncached_prompt_tokens, 300);
    assert_eq!(usage.completion_tokens, 50);
    assert_eq!(usage.cache_hit_ratio, Some(0.75));
    assert!(usage.cache_reporting_available);
}

/// A provider that omits cached tokens is "unavailable", not `0%`; an
/// explicit zero is reported as available.
#[test]
fn prompt_footprint_cache_availability_distinguishes_omitted_field() {
    let _snapshot = snapshot_test_guard();
    let manager = manager_with(vec![]);
    let dynamic = json!({ "role": "user", "content": "env" });
    let messages = manager.build_messages_for_chat(dynamic.clone());
    let tools: Vec<Value> = Vec::new();

    record_request(snapshot_request(
        PromptSurface::Interactive,
        1,
        McpToolMode::Eager,
        &messages,
        &tools,
        &manager,
        &dynamic,
        None,
    ));
    attach_provider_usage(&json!({
        "usage": { "prompt_tokens": 400, "completion_tokens": 10 }
    }));
    let omitted = last_request().expect("recorded");
    let usage = omitted.provider_usage.expect("usage attached");
    assert_eq!(usage.cached_prompt_tokens, 0);
    assert!(!usage.cache_reporting_available);

    record_request(snapshot_request(
        PromptSurface::Interactive,
        2,
        McpToolMode::Eager,
        &messages,
        &tools,
        &manager,
        &dynamic,
        None,
    ));
    attach_provider_usage(&json!({
        "usage": {
            "prompt_tokens": 400,
            "completion_tokens": 10,
            "prompt_tokens_details": { "cached_tokens": 0 },
        }
    }));
    let explicit = last_request().expect("recorded");
    assert!(
        explicit
            .provider_usage
            .expect("usage")
            .cache_reporting_available
    );
}

/// Recording happens before the provider call, so a request that never gets
/// a response still leaves its local sizes behind.
#[test]
fn prompt_footprint_record_without_usage_keeps_local_sizes() {
    let _snapshot = snapshot_test_guard();
    let manager = manager_with(vec![json!({ "role": "user", "content": "hi" })]);
    let dynamic = json!({ "role": "user", "content": "env" });
    let messages = manager.build_messages_for_chat(dynamic.clone());

    record_request(snapshot_request(
        PromptSurface::Agent,
        3,
        McpToolMode::Bridge,
        &messages,
        &[],
        &manager,
        &dynamic,
        None,
    ));

    let recorded = last_request().expect("recorded");
    assert_eq!(recorded.surface, PromptSurface::Agent);
    assert_eq!(recorded.iteration, 3);
    assert!(recorded.messages_json_bytes > 0);
    assert!(recorded.provider_usage.is_none());
}

/// Diagnostics never ask about project skills: untrusted roots are counted
/// as pending and excluded from the measured prompt, with no approval
/// prompt recorded.
#[test]
fn prompt_footprint_never_asks_about_project_skills() {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let skill_dir = root.join(".dogesh").join("skills").join("deploy");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: deploy\ndescription: repo deploy steps\n---\n",
    )
    .unwrap();

    let previous = std::env::var_os("XDG_STATE_HOME");
    // SAFETY: single-threaded under `env_lock`.
    unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
    let measure = || {
        let mut proxy = crate::test_support::TestShellProxy {
            current_dir: root.clone(),
            confirm_result: false,
            ..crate::test_support::TestShellProxy::default()
        };
        let fixed = fixed_prompt_footprint(&mut proxy);
        (fixed, proxy.confirm_calls)
    };
    let result = measure();
    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
    }

    let (fixed, confirm_calls) = result;
    assert_eq!(fixed.project_skill_roots_pending_trust, 1);
    assert_eq!(confirm_calls, 0, "diagnostics must not prompt");
}

#[derive(serde::Deserialize)]
struct FootprintBudget {
    version: u32,
    max_base_system_prompt_bytes: usize,
    max_interactive_core_tools_json_bytes: usize,
    max_agent_core_tools_json_bytes: usize,
    max_mcp_bridge_tools_json_bytes: usize,
}

fn canonical_footprint() -> (usize, usize, usize, usize) {
    let mcp = McpManager::default();
    let base = build_system_prompt(None, None, &mcp, &[], McpToolMode::Eager)
        .text
        .len();
    let handle = Arc::new(RwLock::new(mcp));
    let exposure = ToolSearchExposure::default();
    let (interactive_base, _) = split_turn_tool_bases(&handle, false, McpToolMode::Eager);
    let interactive = serde_json::to_vec(&build_request_tools(
        McpToolMode::Eager,
        &interactive_base,
        &[],
        &handle,
        &exposure,
    ))
    .unwrap()
    .len();
    let (_, agent) = split_turn_tool_bases(&handle, true, McpToolMode::Eager);
    let agent = serde_json::to_vec(&agent).unwrap().len();
    let trio = serde_json::to_vec(&tool::mcp_bridge::bridge_definitions())
        .unwrap()
        .len();
    (base, interactive, agent, trio)
}

/// Prints the current canonical values for re-baselining `budget.json`.
/// Run with `-- --nocapture`; reductions never need this, only intentional
/// growth does.
#[test]
fn prompt_footprint_report_canonical_values_for_baselining() {
    let (base, interactive, agent, trio) = canonical_footprint();
    eprintln!("canonical footprint: base_system_prompt_bytes={base}");
    eprintln!("canonical footprint: interactive_core_tools_json_bytes={interactive}");
    eprintln!("canonical footprint: agent_core_tools_json_bytes={agent}");
    eprintln!("canonical footprint: mcp_bridge_tools_json_bytes={trio}");
}

/// One-way ratchet: the canonical fixed footprint may shrink freely, but
/// growth beyond the committed maxima fails until `budget.json` is updated
/// in review. User-configurable content (skills, operator prompt, project
/// skills, live MCP catalogs) is never part of this limit.
#[test]
fn prompt_footprint_stays_within_the_committed_budget() {
    let budget: FootprintBudget =
        serde_json::from_str(include_str!("budget.json")).expect("valid budget fixture");
    assert_eq!(budget.version, FOOTPRINT_REPORT_VERSION);

    let (base, interactive, agent, trio) = canonical_footprint();
    assert!(
        base <= budget.max_base_system_prompt_bytes,
        "base system prompt grew to {base} bytes (max {})",
        budget.max_base_system_prompt_bytes
    );
    assert!(
        interactive <= budget.max_interactive_core_tools_json_bytes,
        "interactive tools grew to {interactive} bytes (max {})",
        budget.max_interactive_core_tools_json_bytes
    );
    assert!(
        agent <= budget.max_agent_core_tools_json_bytes,
        "agent tools grew to {agent} bytes (max {})",
        budget.max_agent_core_tools_json_bytes
    );
    assert!(
        trio <= budget.max_mcp_bridge_tools_json_bytes,
        "bridge trio grew to {trio} bytes (max {})",
        budget.max_mcp_bridge_tools_json_bytes
    );
}
