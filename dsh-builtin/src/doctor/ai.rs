//! `doctor ai` / `doctor mcp`: AI provider configuration, chat session,
//! runtime skill footprint, configured MCP servers, and the prompt footprint
//! profiler (`doctor ai --prompt-size`).
//!
//! Local footprint figures are compact JSON / UTF-8 bytes measured offline;
//! only provider-reported usage may be called token counts.
use crate::McpToolMode;
use crate::ShellProxy;
use dsh_types::Context;
use dsh_types::mcp::McpTransport;

use super::*;

fn mcp_mode_str(mode: McpToolMode) -> &'static str {
    match mode {
        McpToolMode::Eager => "eager",
        McpToolMode::Bridge => "bridge",
    }
}

/// Report the timeout the client resolves, clamps included.
pub(super) fn ai_timeout_secs(proxy: &mut dyn ShellProxy) -> u64 {
    crate::chatgpt::load_openai_config(proxy)
        .timeout()
        .as_secs()
}

pub(super) fn check_ai(ctx: &Context, proxy: &mut dyn ShellProxy) {
    // Report what the client will actually use. Re-deriving the model, the base
    // URL and their defaults here meant `doctor` drifted from `OpenAiConfig`
    // and, because it skipped `sanitize_base_url`, called an `http://` endpoint
    // "ok" while every request went to api.openai.com instead.
    let config = crate::chatgpt::load_openai_config(proxy);
    let configured_base_url = proxy
        .get_var("AI_CHAT_BASE_URL")
        .or_else(|| proxy.get_var("OPENAI_BASE_URL"));
    let lang = proxy
        .get_var("AI_MESSAGE_LANG")
        .unwrap_or_else(|| "default".to_string());

    let _ = ctx.write_stdout(&format!("ok provider {}", config.provider_name()));
    if let Err(error) = config.readiness() {
        let _ = ctx.write_stdout(&format!("warn readiness {error}"));
    }
    let key_state = if config.readiness().is_ok() {
        "ok"
    } else {
        "warn"
    };
    let _ = ctx.write_stdout(&format!(
        "{key_state} api-key {}",
        mask_secret(config.api_key().map(str::to_string))
    ));
    let _ = ctx.write_stdout(&format!("ok model {}", config.default_model()));
    let _ = ctx.write_stdout(&format!("ok base-url {}", config.base_url()));
    if config.provider() == dsh_openai::AiProvider::ApiKey
        && let Some(configured) = configured_base_url.as_deref()
        && configured.trim_end_matches('/') != config.base_url()
    {
        let _ = ctx.write_stdout(&format!(
            "warn base-url configured {configured} replaced; set AI_CHAT_ALLOW_INSECURE_HTTP=1 to keep it"
        ));
    }
    let _ = ctx.write_stdout(&format!("ok message-lang {lang}"));

    let _ = ctx.write_stdout(&format!("ok request-timeout {}s", ai_timeout_secs(proxy)));
    // Unset does not mean "nothing is ever sent": `build_body` still defaults
    // `tools` requests to `reasoning_effort: none` for a known OpenAI
    // reasoning model, including the shell's own default `gpt-5-mini`. Ask
    // `is_openai_reasoning_model` rather than naming the family here, so this
    // (a) only shows the caveat for a model it actually applies to and
    // (b) can't drift from the prefix list `dsh-openai` matches against.
    let reasoning_effort_line = match config.reasoning_effort() {
        Some(value) => value.to_string(),
        None if dsh_openai::is_openai_reasoning_model(config.default_model()) => {
            "default (auto \"none\" on tools requests to this model)".to_string()
        }
        None => "default".to_string(),
    };
    let _ = ctx.write_stdout(&format!("ok reasoning-effort {reasoning_effort_line}"));

    match crate::chatgpt::chat_session_description(proxy) {
        Some(detail) => {
            let _ = ctx.write_stdout(&format!("ok chat-session {detail}"));
        }
        None => {
            let _ = ctx.write_stdout("skip chat-session none carried");
        }
    }

    let usage = dsh_openai::usage::session_total();
    if usage.is_empty() {
        let _ = ctx.write_stdout("skip token-usage no AI requests in this session");
    } else {
        let _ = ctx.write_stdout(&format!("ok token-usage {}", usage.summary_line()));
    }

    let dsh_skills_dir = Some(crate::config_paths::skills_dir());
    let dsh_skill_count = match dsh_skills_dir.as_ref() {
        Some(path) if path.exists() => {
            let count = count_skill_dirs(path);
            let _ = ctx.write_stdout(&format!(
                "ok dsh-runtime-skills {} entries={count}",
                path.display()
            ));
            count
        }
        Some(path) => {
            let _ = ctx.write_stdout(&format!(
                "skip dsh-runtime-skills missing {}",
                path.display()
            ));
            0
        }
        None => {
            let _ = ctx.write_stdout("warn dsh-runtime-skills unable-to-determine-config-dir");
            0
        }
    };

    let codex_skills_dir = codex_runtime_skills_dir(proxy);
    let codex_skill_count = match codex_skills_dir.as_ref() {
        Some(path) if path.exists() => {
            let count = count_skill_dirs(path);
            let _ = ctx.write_stdout(&format!(
                "ok codex-runtime-skills {} entries={count}",
                path.display()
            ));
            count
        }
        Some(path) => {
            let _ = ctx.write_stdout(&format!(
                "skip codex-runtime-skills missing {}",
                path.display()
            ));
            0
        }
        None => {
            let _ = ctx.write_stdout("warn codex-runtime-skills unable-to-determine-home-dir");
            0
        }
    };

    if dsh_skill_count + codex_skill_count > 8 {
        let _ = ctx.write_stdout(
            "warn runtime-skill-footprint high consider installing only the skills needed for this repository",
        );
    } else {
        let _ = ctx.write_stdout("ok runtime-skill-footprint minimal");
    }
}

/// The offline prompt profiler behind `doctor ai --prompt-size`.
///
/// No API key, no network, no prompts: system-prompt sections, both tool
/// surfaces, the MCP catalog, and the last main-loop request, if this
/// process has sent one.
pub(super) fn check_prompt_footprint(ctx: &Context, proxy: &mut dyn ShellProxy) {
    use crate::chatgpt::footprint;

    let _ = ctx.write_stdout(
        "Local footprint is compact JSON / UTF-8 bytes; token counts are provider-reported only.",
    );
    let fixed = footprint::fixed_prompt_footprint(proxy);

    let _ = ctx.write_stdout(&format!("ok mcp-mode {}", mcp_mode_str(fixed.mcp_mode)));
    if fixed.project_skill_roots_pending_trust > 0 {
        let _ = ctx.write_stdout(&format!(
            "warn project-skills {} root(s) pending trust; excluded from the measured prompt",
            fixed.project_skill_roots_pending_trust
        ));
    }

    let system = &fixed.system_prompt;
    let _ = ctx.write_stdout("Fixed system prompt");
    for (label, bytes) in [
        ("total", system.total_text_bytes),
        ("base guidance", system.base_guidance_bytes),
        ("skills index", system.skills_bytes),
        ("MCP guidance", system.mcp_guidance_bytes),
        ("operator instructions", system.operator_bytes),
        ("language", system.language_bytes),
    ] {
        let _ = ctx.write_stdout(&format!("  {label:<22} {bytes} B"));
    }

    let _ = ctx.write_stdout("Tool schemas");
    for (label, surface) in [
        ("interactive", &fixed.interactive_tools),
        ("agent", &fixed.agent_tools),
    ] {
        let _ = ctx.write_stdout(&format!(
            "  {label:<22} {} B / {} tools",
            surface.tools_json_bytes, surface.count
        ));
    }

    let catalog = &fixed.mcp_catalog;
    let _ = ctx.write_stdout("MCP catalog");
    let _ = ctx.write_stdout(&format!(
        "  {:<22} {} B / {} tools",
        "discoverable", catalog.discoverable_direct_schema_bytes, catalog.discoverable_tools
    ));
    let _ = ctx.write_stdout(&format!(
        "  {:<22} {} B / {} tools",
        "active", catalog.active_direct_schema_bytes, catalog.active_tools
    ));
    let _ = ctx.write_stdout(&format!(
        "  {:<22} {} B / {} tools",
        "model surface", catalog.model_surface_bytes, catalog.model_surface_tools
    ));

    // Largest definitions across both surfaces, highest bytes first.
    let _ = ctx.write_stdout("Largest tool schemas");
    let mut largest: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for surface in [&fixed.interactive_tools, &fixed.agent_tools] {
        for definition in &surface.tools {
            largest
                .entry(definition.name.as_str())
                .and_modify(|bytes| *bytes = (*bytes).max(definition.json_bytes))
                .or_insert(definition.json_bytes);
        }
    }
    let mut largest: Vec<(&str, usize)> = largest.into_iter().collect();
    largest.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    for (name, bytes) in largest.iter().take(10) {
        let _ = ctx.write_stdout(&format!("  {name:<22} {bytes} B"));
    }

    match footprint::last_request() {
        None => {
            let _ =
                ctx.write_stdout("skip last-request no main chat request has run in this process");
        }
        Some(request) => {
            let surface = match request.surface {
                footprint::PromptSurface::Interactive => "interactive",
                footprint::PromptSurface::Agent => "agent",
            };
            let _ = ctx.write_stdout(&format!(
                "Last main request ({surface}, iteration {}, mode {})",
                request.iteration,
                mcp_mode_str(request.mcp_mode)
            ));
            let conversation = &request.conversation;
            for (label, bytes) in [
                ("messages", request.messages_json_bytes),
                ("tools", request.tools_json_bytes),
                ("context", request.context_json_bytes),
                ("pinned goal", conversation.pinned_goal_json_bytes),
                ("summary", conversation.summary_json_bytes),
                ("conversation user", conversation.user_json_bytes),
                ("conversation assistant", conversation.assistant_json_bytes),
                ("tool results", conversation.tool_result_json_bytes),
                ("dynamic context", request.dynamic_context_json_bytes),
                (
                    "agent runtime context",
                    request.agent_runtime_context_json_bytes,
                ),
            ] {
                let _ = ctx.write_stdout(&format!("  {label:<22} {bytes} B"));
            }
            let observations = &request.observations;
            let _ = ctx.write_stdout("Observations");
            let _ = ctx.write_stdout(&format!(
                "  {:<22} {} / {} B",
                "stored", observations.stored_entries, observations.stored_content_bytes
            ));
            let _ = ctx.write_stdout(&format!(
                "  {:<22} {}",
                "active references", observations.active_references
            ));
            let _ = ctx.write_stdout(&format!(
                "  {:<22} {} B",
                "original messages", observations.active_original_message_bytes
            ));
            let _ = ctx.write_stdout(&format!(
                "  {:<22} {} B",
                "active stubs", observations.active_stub_message_bytes
            ));
            let _ = ctx.write_stdout(&format!(
                "  {:<22} {} B",
                "active context reclaimed", observations.active_reclaimed_json_bytes
            ));
            match &request.provider_usage {
                None => {
                    let _ = ctx
                        .write_stdout("skip provider-usage no usage reported for the last request");
                }
                Some(usage) => {
                    let _ = ctx.write_stdout("Provider usage");
                    let _ = ctx.write_stdout(&format!(
                        "  {:<22} {} tokens",
                        "prompt", usage.prompt_tokens
                    ));
                    let _ = ctx.write_stdout(&format!(
                        "  {:<22} {} tokens",
                        "cached", usage.cached_prompt_tokens
                    ));
                    let _ = ctx.write_stdout(&format!(
                        "  {:<22} {} tokens",
                        "uncached", usage.uncached_prompt_tokens
                    ));
                    match (usage.cache_reporting_available, usage.cache_hit_ratio) {
                        (true, Some(ratio)) => {
                            let _ = ctx.write_stdout(&format!(
                                "  {:<22} {:.1}%",
                                "cache hit",
                                ratio * 100.0
                            ));
                        }
                        _ => {
                            let _ = ctx.write_stdout("  cache hit              unavailable");
                        }
                    }
                    let _ = ctx.write_stdout(&format!(
                        "  {:<22} {} tokens",
                        "completion", usage.completion_tokens
                    ));
                }
            }
        }
    }
}

pub(super) fn check_mcp(ctx: &Context, proxy: &mut dyn ShellProxy) {
    use crate::chatgpt::footprint;

    let configured_servers = proxy.list_mcp_servers();
    let configured = configured_servers.len();
    // The authoritative manager, not the `MCP_*` display projections: the
    // same state the chat runtime resolves its tool surface from.
    let handle = proxy.agent_mcp_manager();
    let manager = handle.read();
    let status = footprint::mcp_footprint_status_from(proxy, &manager);
    let mode = status.mode;
    let active_groups = manager
        .tool_groups()
        .iter()
        .filter(|group| group.enabled && !manager.is_disabled(&group.name))
        .count();

    let state = if configured > 0 { "ok" } else { "warn" };
    let _ = ctx.write_stdout(&format!("{state} configured {configured}"));
    let _ = ctx.write_stdout(&format!("ok connected {}", manager.connected_count()));
    let _ = ctx.write_stdout(&format!("ok tools {}", manager.tool_count()));
    let _ = ctx.write_stdout(&format!("ok active-tools {}", status.active_tools));
    let _ = ctx.write_stdout(&format!("ok active-groups {active_groups}"));
    let _ = ctx.write_stdout(&format!("ok mcp-mode {}", mcp_mode_str(mode)));
    // The per-turn tax is what the model actually sees, and that depends on
    // the selected mode: bridge mode carries a fixed trio no matter how
    // large the catalog is, so only eager mode warns, and on measured
    // schema bytes rather than on tool count alone.
    if mode == McpToolMode::Bridge {
        let _ = ctx.write_stdout(&format!(
            "ok mcp-tools-footprint mode=bridge discoverable={} direct-schema-bytes={} deferred",
            status.discoverable_tools, status.discoverable_schema_bytes
        ));
    } else if status.warn_over_threshold {
        let _ = ctx.write_stdout(&format!(
            "warn mcp-tools-footprint mode=eager high active={} active-schema-bytes={}; hide idle groups with `mcp group disable <group>` or disconnect idle servers",
            status.active_tools, status.active_schema_bytes
        ));
    } else {
        let _ = ctx.write_stdout(&format!(
            "ok mcp-tools-footprint mode=eager active={} active-schema-bytes={}",
            status.active_tools, status.active_schema_bytes
        ));
    }
    for server in configured_servers {
        if let McpTransport::Sse { url } = &server.transport {
            let _ = ctx.write_stdout(&format!(
                "warn mcp {} sse url={} configuration-only use-streamable-http",
                server.label, url
            ));
        }
    }
}
