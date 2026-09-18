//! `doctor ai` / `doctor mcp`: AI provider configuration, chat session,
//! runtime skill footprint, and configured MCP servers.
use crate::ShellProxy;
use dsh_types::Context;
use dsh_types::mcp::McpTransport;

use super::*;

/// Connected MCP tool count above which `doctor mcp` warns about prompt
/// footprint. Interactive turns carry active definitions in full on
/// every round (plus turn-local `tool_search` hits), so
/// the count is a per-turn tax. A heuristic like the runtime-skill-footprint
/// limit in `check_ai`, not a measured token budget: schemas vary in size.
const MCP_TOOLS_FOOTPRINT_WARN: usize = 20;
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

    let key_state = if config.api_key().is_some() {
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
    if let Some(configured) = configured_base_url.as_deref()
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

pub(super) fn check_mcp(ctx: &Context, proxy: &mut dyn ShellProxy) {
    let configured_servers = proxy.list_mcp_servers();
    let configured = configured_servers.len();
    let servers = proxy
        .get_var("MCP_SERVERS")
        .unwrap_or_else(|| configured.to_string());
    let connected = proxy
        .get_var("MCP_CONNECTED")
        .unwrap_or_else(|| "0".to_string());
    let tools = proxy
        .get_var("MCP_TOOLS")
        .unwrap_or_else(|| "0".to_string());
    let active_tools = proxy
        .get_var("MCP_ACTIVE_TOOLS")
        .unwrap_or_else(|| tools.clone());
    let active_groups = proxy
        .get_var("MCP_ACTIVE_GROUPS")
        .unwrap_or_else(|| "0".to_string());

    let state = if configured > 0 { "ok" } else { "warn" };
    let _ = ctx.write_stdout(&format!("{state} configured {configured}"));
    let _ = ctx.write_stdout(&format!("ok servers {servers}"));
    let _ = ctx.write_stdout(&format!("ok connected {connected}"));
    let _ = ctx.write_stdout(&format!("ok tools {tools}"));
    let _ = ctx.write_stdout(&format!("ok active-tools {active_tools}"));
    let _ = ctx.write_stdout(&format!("ok active-groups {active_groups}"));
    // The per-turn tax is what the model actually sees, not what is
    // registered: disabling idle groups quiets this warning.
    let active_count: usize = active_tools.parse().unwrap_or(0);
    if active_count > MCP_TOOLS_FOOTPRINT_WARN {
        let _ = ctx.write_stdout(&format!(
            "warn mcp-tools-footprint high tools={active_count} interactive turns carry all active definitions; hide idle groups with `mcp group disable <group>` or disconnect idle servers"
        ));
    } else {
        let _ = ctx.write_stdout(&format!("ok mcp-tools-footprint tools={active_count}"));
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
