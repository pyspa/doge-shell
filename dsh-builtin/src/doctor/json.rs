//! `doctor --json` report assembly: one function per section, mirroring the
//! human-readable `check_*` functions in the sibling section modules.
use crate::ShellProxy;
use crate::project_context;
use crate::task;
use dsh_types::mcp::McpTransport;
use dsh_types::{Context, ExitStatus};
use serde_json::json;
use std::fs;
use std::path::Path;

use super::*;
pub(super) fn print_json_report(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    current_dir: &Path,
    section: Option<&str>,
) -> ExitStatus {
    let project = project_context::resolve_project_context(current_dir);
    let details = json_section_details(proxy, current_dir, section);
    let runtimes = project
        .runtimes
        .iter()
        .map(|runtime| {
            json!({
                "name": runtime.name,
                "source": runtime.source,
                "version": runtime.version,
                "path": runtime.path
            })
        })
        .collect::<Vec<_>>();
    let tasks = task::summarize_tasks_in_dir_metadata_only(&project.project_root)
        .map(|summary| {
            json!({
                "count": summary.tasks.len(),
                "deferred_sources": summary.deferred_sources
            })
        })
        .unwrap_or_else(|err| json!({"error": err.to_string()}));
    let report = json!({
        "section": section.unwrap_or("all"),
        "cwd": current_dir,
        "project": {
            "root": project.project_root,
            "markers": project.project_markers,
            "runtimes": runtimes,
            "tasks": tasks
        },
        "integrations": {
            "mcp_servers": proxy.list_mcp_servers().len(),
            // Through the same resolver as the `ai` section, which also knows
            // about `OPEN_AI_API_KEY`; the two used to disagree.
            "ai_configured": crate::chatgpt::load_openai_config(proxy).api_key().is_some()
        },
        "details": details
    });
    match serde_json::to_string(&report) {
        Ok(output) => {
            let _ = ctx.write_stdout(&output);
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!("doctor: JSON serialization failed: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}

pub(super) fn json_section_details(
    proxy: &mut dyn ShellProxy,
    current_dir: &Path,
    section: Option<&str>,
) -> serde_json::Value {
    match section {
        Some("config") => {
            let config = crate::config_paths::config_file("config.lisp");
            let skills = crate::config_paths::skills_dir();
            json!({
                "config": json!({"path": config, "exists": config.is_file()}),
                "runtime_skills": json!({
                    "path": skills,
                    "exists": skills.is_dir(),
                    "entries": fs::read_dir(&skills).map(|entries| entries.count()).unwrap_or(0)
                })
            })
        }
        Some("ai") => {
            let usage = dsh_openai::usage::session_total();
            let config = crate::chatgpt::load_openai_config(proxy);
            json!({
            "configured": config.api_key().is_some(),
            "model": config.default_model(),
            "base_url": config.base_url(),
            "message_lang": proxy.get_var("AI_MESSAGE_LANG").unwrap_or_else(|| "default".to_string()),
            "timeout_secs": config.timeout().as_secs(),
            "reasoning_effort": config.reasoning_effort().unwrap_or("default"),
            "reasoning_effort_auto_none_for_tools": config.reasoning_effort().is_none()
                && dsh_openai::is_openai_reasoning_model(config.default_model()),
            "usage": {
                "requests": usage.requests,
                "prompt_tokens": usage.prompt_tokens,
                "cached_prompt_tokens": usage.cached_prompt_tokens,
                "completion_tokens": usage.completion_tokens
            }
            })
        }
        Some("mcp") => {
            let servers = proxy.list_mcp_servers();
            json!({
                "configured": servers.len(),
                "servers": servers.iter().map(|server| {
                    let transport = match &server.transport {
                        McpTransport::Stdio { .. } => "stdio",
                        McpTransport::Sse { .. } => "sse",
                        McpTransport::Http { .. } => "http",
                    };
                    json!({"label": server.label, "transport": transport})
                }).collect::<Vec<_>>()
            })
        }
        Some("project") => {
            let project = project_context::resolve_project_context(current_dir);
            json!({
                "root": project.project_root,
                "markers": project.project_markers,
                "activations": project.activations.iter().map(|activation| {
                    json!({"kind": activation.kind, "path": activation.path})
                }).collect::<Vec<_>>(),
                "completion": proxy.completion_diagnostics()
            })
        }
        Some("runtime" | "runtimes") => {
            let commands = [
                "mise", "direnv", "rustc", "cargo", "node", "npm", "pnpm", "python3", "uv", "go",
                "just",
            ]
            .into_iter()
            .map(|command| {
                json!({
                    "command": command,
                    "path": resolve_in_path(command),
                    "version": read_version(command)
                })
            })
            .collect::<Vec<_>>();
            json!({"commands": commands})
        }
        Some("performance" | "perf") => json!({
            "history_entries": proxy.command_history_len(),
            "executable_cache_entries": proxy.executable_cache_len(),
            "completion": proxy.completion_diagnostics()
        }),
        Some("safety") => json_safety_details(proxy, current_dir),
        Some("dev" | "validate") => json_dev_details(current_dir),
        Some("hooks") => json_hooks_details(proxy),
        Some("skills") => {
            let dsh = crate::config_paths::skills_dir();
            let codex = codex_runtime_skills_dir(proxy);
            let project = crate::chatgpt::skills::project_skills_root(current_dir);
            let project_agents = crate::chatgpt::skills::project_agents_skills_root(current_dir);
            json!({
                "dsh_runtime": json!({"path": dsh, "entries": count_skill_dirs(&dsh)}),
                "project_runtime": project.as_ref().map(|path| json!({"path": path, "entries": count_skill_dirs(path)})),
                // The interop root feeds the prompt as well; tooling reading
                // only `project_runtime` concluded a checkout shipped none.
                "project_agents_runtime": project_agents.as_ref().map(|path| json!({"path": path, "entries": count_skill_dirs(path)})),
                "codex_runtime": codex.as_ref().map(|path| json!({"path": path, "entries": count_skill_dirs(path)}))
            })
        }
        Some("setup") => {
            let root = crate::config_paths::config_home();
            json!({"config_root": json!({"path": root, "exists": root.is_dir()})})
        }
        None => json!({"kind": "summary"}),
        Some(_) => serde_json::Value::Null,
    }
}

pub(super) fn json_safety_details(
    proxy: &mut dyn ShellProxy,
    current_dir: &Path,
) -> serde_json::Value {
    let allowlist = proxy.list_execute_allowlist();
    let servers = proxy.list_mcp_servers();
    let project = project_context::resolve_project_context(current_dir);
    let envrc = project.project_root.join(".envrc");
    let base_url = proxy.get_var("AI_CHAT_BASE_URL");
    let base_url_safe = base_url.as_deref().is_none_or(is_https_or_local_http_url);
    let envrc_exists = envrc.is_file();
    let envrc_allowed = !envrc_exists || proxy.is_direnv_allowed(&project.project_root);
    json!({
        "execute_allowlist": allowlist.iter().map(|entry| json!({
            "entry": entry,
            "risky": is_risky_execute_allowlist_entry(entry)
        })).collect::<Vec<_>>(),
        "mcp": {
            "configured": servers.len(),
            "sse_servers": servers.iter().filter(|server| matches!(&server.transport, McpTransport::Sse { .. })).count()
        },
        "ai_base_url": {
            "value": base_url,
            "safe": base_url_safe
        },
        "envrc": {
            "path": envrc,
            "exists": envrc_exists,
            "allowed": envrc_allowed
        }
    })
}

pub(super) fn json_dev_details(current_dir: &Path) -> serde_json::Value {
    let Some(repo_root) = find_repo_root(current_dir) else {
        return json!({"error": "repo-root-not-found"});
    };
    match changed_paths(&repo_root) {
        Ok(paths) => json!({
            "repo_root": repo_root,
            "changed_files": paths,
            "commands": validation_commands_for_paths(&paths),
            "notes": notes_for_paths(&paths)
        }),
        Err(err) => json!({"repo_root": repo_root, "error": err}),
    }
}

pub(super) fn json_hooks_details(proxy: &mut dyn ShellProxy) -> serde_json::Value {
    use crate::chatgpt::hooks::config;

    // The same gates the text report applies. Emitting the hook list while none
    // of them can fire told a script the checks were running.
    if config::nested_in_a_hook() {
        return json!({"config": null, "hooks": [], "disabled_by": config::HOOK_DEPTH_ENV});
    }
    if !config::enabled(proxy) {
        return json!({"config": null, "hooks": [], "disabled_by": config::HOOKS_ENABLED_KEY});
    }

    // Reported as-resolved, and an unparseable value is shown rather than
    // swallowed: it stops the chat, so it has to be visible here.
    let turn_budget = match config::turn_budget_ms(proxy) {
        Ok(value) => json!(value),
        Err(err) => json!(err),
    };
    let Some(path) = config::config_path(proxy) else {
        return json!({"config": null, "hooks": [], "turn_budget_ms": turn_budget});
    };
    match config::read(&path) {
        Ok(hooks) => json!({
            "config": path,
            "turn_budget_ms": turn_budget,
            "hooks": hooks.all().iter().map(|hook| json!({
                "id": hook.id,
                "events": hook.events.iter().map(|event| event.as_str()).collect::<Vec<_>>(),
                "enabled": hook.enabled,
                "timeout_ms": hook.timeout_ms(),
                "command_found": program_is_runnable(&hook.command[0]),
                "match": {
                    "tools": hook.matcher.as_ref().map(|m| m.tools.clone()).unwrap_or_default(),
                    "programs": hook.matcher.as_ref().map(|m| m.programs.clone()).unwrap_or_default(),
                    "paths": hook.matcher.as_ref().map(|m| m.paths.clone()).unwrap_or_default(),
                    "arguments": hook.matcher.as_ref().map(|m| m.arguments.clone()).unwrap_or_default(),
                },
            })).collect::<Vec<_>>()
        }),
        Err(err) => json!({"config": path, "error": err}),
    }
}
