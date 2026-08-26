use crate::ShellProxy;
use crate::project_context;
use crate::safety_policy;
use crate::task;
use dsh_types::mcp::McpTransport;
use dsh_types::{Context, ExitStatus};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PERFORMANCE_TOP_DEFAULT: usize = 5;

pub fn description() -> &'static str {
    "Diagnose config, AI, MCP, project, runtime, skills, safety, setup, and dev validation state"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let json_output = argv.iter().skip(1).any(|value| value == "--json");
    let section = argv
        .iter()
        .skip(1)
        .find(|value| value.as_str() != "--json")
        .map(|value| value.as_str());
    if matches!(section, Some("-h" | "--help" | "help")) {
        return print_help(ctx);
    }
    if section.is_some_and(|value| !is_known_section(value)) {
        let _ = ctx.write_stderr(&format!(
            "doctor: unknown section `{}`. Use `doctor --help`.",
            section.unwrap_or_default()
        ));
        return ExitStatus::ExitedWith(1);
    }

    let current_dir = proxy
        .get_current_dir()
        .unwrap_or_else(|_| PathBuf::from("."));

    if json_output {
        if matches!(section, Some("fix")) {
            let _ = ctx.write_stderr("doctor: fix cannot be combined with --json");
            return ExitStatus::ExitedWith(1);
        }
        return print_json_report(ctx, proxy, &current_dir, section);
    }

    if matches!(section, Some("setup" | "fix")) {
        print_header(ctx, "setup");
        check_setup(ctx, proxy, &current_dir, section == Some("fix"));
        return ExitStatus::ExitedWith(0);
    }

    if show_section(section, "config") {
        print_header(ctx, "config");
        check_config(ctx);
    }
    if show_section(section, "ai") {
        print_header(ctx, "ai");
        check_ai(ctx, proxy);
    }
    if show_section(section, "mcp") {
        print_header(ctx, "mcp");
        check_mcp(ctx, proxy);
    }
    if show_section(section, "project") {
        print_header(ctx, "project");
        check_project(ctx, proxy, &current_dir);
    }
    if show_section(section, "runtime") || show_section(section, "runtimes") {
        print_header(ctx, "runtimes");
        check_runtimes(ctx);
    }
    if show_section(section, "performance") || show_section(section, "perf") {
        print_header(ctx, "performance");
        check_performance(ctx, proxy, argv.get(2..).unwrap_or(&[]));
    }
    if show_section(section, "skills") {
        print_header(ctx, "skills");
        check_skills(ctx, proxy, &current_dir);
    }
    if show_section(section, "safety") {
        print_header(ctx, "safety");
        check_safety(ctx, proxy, &current_dir);
    }
    if show_section(section, "dev") || show_section(section, "validate") {
        print_header(ctx, "dev");
        check_dev(ctx, &current_dir);
    }

    ExitStatus::ExitedWith(0)
}

fn print_json_report(
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
            "ai_configured": proxy.get_var("AI_CHAT_API_KEY")
                .or_else(|| proxy.get_var("OPENAI_API_KEY"))
                .is_some_and(|value| !value.trim().is_empty())
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

fn json_section_details(
    proxy: &mut dyn ShellProxy,
    current_dir: &Path,
    section: Option<&str>,
) -> serde_json::Value {
    match section {
        Some("config") => {
            let config_root = dirs::config_dir().map(|path| path.join("dsh"));
            let config = config_root.as_ref().map(|path| path.join("config.lisp"));
            let skills = config_root.as_ref().map(|path| path.join("skills"));
            json!({
                "config": config.as_ref().map(|path| json!({"path": path, "exists": path.is_file()})),
                "runtime_skills": skills.as_ref().map(|path| json!({
                    "path": path,
                    "exists": path.is_dir(),
                    "entries": fs::read_dir(path).map(|entries| entries.count()).unwrap_or(0)
                }))
            })
        }
        Some("ai") => {
            let usage = dsh_openai::usage::session_total();
            json!({
            "configured": proxy.get_var("AI_CHAT_API_KEY")
                .or_else(|| proxy.get_var("OPENAI_API_KEY"))
                .or_else(|| proxy.get_var("OPEN_AI_API_KEY"))
                .is_some_and(|value| !value.trim().is_empty()),
            "model": proxy.get_var("AI_CHAT_MODEL")
                .or_else(|| proxy.get_var("OPENAI_MODEL"))
                .unwrap_or_else(|| "gpt-5-mini".to_string()),
            "base_url": proxy.get_var("AI_CHAT_BASE_URL")
                .or_else(|| proxy.get_var("OPENAI_BASE_URL"))
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            "message_lang": proxy.get_var("AI_MESSAGE_LANG").unwrap_or_else(|| "default".to_string()),
            "timeout_secs": ai_timeout_secs(proxy),
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
        Some("skills") => {
            let dsh = dirs::config_dir().map(|path| path.join("dsh/skills"));
            let codex = dirs::home_dir().map(|path| path.join(".codex/skills"));
            json!({
                "dsh_runtime": dsh.as_ref().map(|path| json!({"path": path, "entries": count_skill_dirs(path)})),
                "codex_runtime": codex.as_ref().map(|path| json!({"path": path, "entries": count_skill_dirs(path)}))
            })
        }
        Some("setup") => {
            let root = dirs::config_dir().map(|path| path.join("dsh"));
            json!({"config_root": root.as_ref().map(|path| json!({"path": path, "exists": path.is_dir()}))})
        }
        None => json!({"kind": "summary"}),
        Some(_) => serde_json::Value::Null,
    }
}

fn json_safety_details(proxy: &mut dyn ShellProxy, current_dir: &Path) -> serde_json::Value {
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

fn json_dev_details(current_dir: &Path) -> serde_json::Value {
    let Some(repo_root) = find_repo_root(current_dir) else {
        return json!({"error": "repo-root-not-found"});
    };
    match changed_paths(&repo_root) {
        Ok(paths) => json!({
            "repo_root": repo_root,
            "changed_files": paths,
            "commands": validation_commands_for_paths(&paths)
        }),
        Err(err) => json!({"repo_root": repo_root, "error": err}),
    }
}

fn print_help(ctx: &Context) -> ExitStatus {
    let _ = ctx.write_stdout(help_text());
    ExitStatus::ExitedWith(0)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: doctor [config|ai|mcp|project|runtime|performance|skills|safety|setup|fix|dev|validate] [OPTIONS]\n",
        "\n",
        "Run diagnostics for the current shell setup. Without a section, all checks run.\n",
        "\n",
        "Sections:\n",
        "  config   Check config.lisp and runtime skills directory\n",
        "  ai       Check AI-related environment and defaults\n",
        "  mcp      Check configured MCP servers and connection counters\n",
        "  project  Detect project marker files in the current directory\n",
        "  runtime  Check common developer tools in PATH\n",
        "  performance  Show command timing and runtime skill scan state\n",
        "  skills   Compare repo-local skills with expected runtime skills\n",
        "  safety   Check AI tool, MCP, direnv, log, and allowlist safety posture\n",
        "  setup    Show first-run setup state and recommended next steps\n",
        "  fix      Create safe missing setup directories/files, then show setup state\n",
        "  dev      Suggest validation commands from changed files\n",
        "  validate Alias for dev\n",
        "\n",
        "Examples:\n",
        "  doctor\n",
        "  doctor ai\n",
        "  doctor project\n",
        "  doctor performance --top 5 --latency --latency-iters 1000\n",
        "  doctor skills\n",
        "  doctor safety\n",
        "  doctor setup\n",
        "  doctor fix\n",
        "  doctor validate\n",
        "  doctor --json\n",
        "  doctor --help\n",
    )
}

fn is_known_section(value: &str) -> bool {
    matches!(
        value,
        "config"
            | "ai"
            | "mcp"
            | "project"
            | "runtime"
            | "runtimes"
            | "performance"
            | "perf"
            | "skills"
            | "safety"
            | "setup"
            | "fix"
            | "dev"
            | "validate"
    )
}

fn show_section(selected: Option<&str>, current: &str) -> bool {
    match selected {
        None => true,
        Some("runtime") if current == "runtimes" => true,
        Some("runtimes") if current == "runtime" => true,
        Some("validate") if current == "dev" => true,
        Some(value) => value == current,
    }
}

fn print_header(ctx: &Context, title: &str) {
    let _ = ctx.write_stdout(&format!("[{title}]"));
}

fn check_setup(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path, fix: bool) {
    let Some(config_root) = dirs::config_dir().map(|path| path.join("dsh")) else {
        let _ = ctx.write_stdout("warn config-root unable-to-determine-config-dir");
        return;
    };

    ensure_setup_dir(ctx, &config_root, "config-root", fix);
    ensure_setup_dir(ctx, &config_root.join("skills"), "runtime-skills", fix);
    ensure_setup_dir(ctx, &config_root.join("completions"), "completion-dir", fix);
    ensure_config_file(ctx, &config_root.join("config.lisp"), fix);

    let api_key = proxy
        .get_var("AI_CHAT_API_KEY")
        .or_else(|| proxy.get_var("OPENAI_API_KEY"))
        .or_else(|| proxy.get_var("OPEN_AI_API_KEY"));
    if api_key
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        let _ = ctx.write_stdout(&format!("ok ai-key {}", mask_secret(api_key)));
    } else {
        let _ = ctx.write_stdout(
            "warn ai-key missing set AI_CHAT_API_KEY or OPENAI_API_KEY to enable AI features",
        );
    }

    let mcp_count = proxy.list_mcp_servers().len();
    if mcp_count == 0 {
        let _ = ctx.write_stdout("skip mcp no configured servers");
    } else {
        let _ = ctx.write_stdout(&format!("ok mcp configured={mcp_count}"));
    }

    let project = project_context::resolve_project_context(current_dir);
    let _ = ctx.write_stdout(&format!(
        "ok project-root {}",
        project.project_root.display()
    ));
    if project.project_markers.is_empty() {
        let _ = ctx.write_stdout("warn project-markers none");
    } else {
        let _ = ctx.write_stdout(&format!(
            "ok project-markers {}",
            project.project_markers.join(", ")
        ));
    }

    if project.activations.is_empty() {
        let _ = ctx.write_stdout("skip activation no .env, .envrc, .venv, or venv found");
    } else {
        for activation in &project.activations {
            let _ = ctx.write_stdout(&format!(
                "ok activation {} {}",
                activation.kind,
                activation.path.display()
            ));
        }
        if project.project_root.join(".envrc").exists()
            && !proxy.is_direnv_allowed(&project.project_root)
        {
            let _ = ctx.write_stdout("warn envrc not allow-listed; use (allow-direnv \"<project-root>\") in config.lisp if trusted");
        }
        let _ = ctx.write_stdout("hint run `pm activate` to apply safe project activation");
    }

    match task::summarize_tasks_in_dir_metadata_only(&project.project_root) {
        Ok(summary) if summary.tasks.is_empty() && summary.deferred_sources.is_empty() => {
            let _ = ctx.write_stdout("skip tasks none detected");
        }
        Ok(summary) => {
            if !summary.tasks.is_empty() {
                let _ = ctx.write_stdout(&format!(
                    "ok tasks metadata-detected={}",
                    summary.tasks.len()
                ));
            }
            if !summary.deferred_sources.is_empty() {
                let _ = ctx.write_stdout(&format!(
                    "skip tasks dynamic-probe sources={} run `task --list` for full detection",
                    summary.deferred_sources.join(", ")
                ));
            }
            let _ = ctx.write_stdout("hint run `task` to select a project task");
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("warn tasks unavailable {err}"));
        }
    }

    let _ = ctx.write_stdout(
        "hint run `help ai`, `help project`, or `help --search <keyword>` to discover commands",
    );
}

fn ensure_setup_dir(ctx: &Context, path: &Path, label: &str, fix: bool) {
    if path.is_dir() {
        let _ = ctx.write_stdout(&format!("ok {label} {}", path.display()));
        return;
    }

    if fix {
        match fs::create_dir_all(path) {
            Ok(()) => {
                let _ = ctx.write_stdout(&format!("fixed {label} created {}", path.display()));
            }
            Err(err) => {
                let _ = ctx.write_stdout(&format!("warn {label} create-failed {err}"));
            }
        }
    } else {
        let _ = ctx.write_stdout(&format!("warn {label} missing {}", path.display()));
    }
}

fn ensure_config_file(ctx: &Context, path: &Path, fix: bool) {
    if path.is_file() {
        let _ = ctx.write_stdout(&format!("ok config {}", path.display()));
        return;
    }

    if !fix {
        let _ = ctx.write_stdout(&format!("warn config missing {}", path.display()));
        return;
    }

    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        let _ = ctx.write_stdout(&format!("warn config parent-create-failed {err}"));
        return;
    }

    match fs::write(path, default_config_lisp()) {
        Ok(()) => {
            let _ = ctx.write_stdout(&format!("fixed config created {}", path.display()));
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("warn config create-failed {err}"));
        }
    }
}

fn default_config_lisp() -> &'static str {
    concat!(
        ";; doge-shell config.lisp\n",
        ";; This file was created by `doctor fix`.\n",
        "\n",
        ";; Common aliases\n",
        "(alias \"ll\" \"ls -alF\")\n",
        "(alias \"la\" \"ls -A\")\n",
        "\n",
        ";; AI execute-tool allowlist for low-risk read-only commands.\n",
        "(chat-execute-clear)\n",
        "(chat-execute-add \"ls\" \"cat\" \"echo\" \"grep\" \"find\")\n",
        "\n",
        ";; Uncomment after reviewing a trusted project root with .envrc.\n",
        ";; (allow-direnv \"/path/to/project\")\n",
    )
}

fn check_config(ctx: &Context) {
    let Some(config_root) = dirs::config_dir().map(|path| path.join("dsh")) else {
        let _ = ctx.write_stdout("warn unable to determine config directory");
        return;
    };

    let config_path = config_root.join("config.lisp");
    if config_path.exists() {
        let _ = ctx.write_stdout(&format!("ok config {}", config_path.display()));
    } else {
        let _ = ctx.write_stdout(&format!("warn missing {}", config_path.display()));
    }

    let skills_dir = config_root.join("skills");
    if skills_dir.exists() {
        let count = fs::read_dir(&skills_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        let _ = ctx.write_stdout(&format!(
            "ok runtime-skills {} entries={count}",
            skills_dir.display()
        ));
    } else {
        let _ = ctx.write_stdout(&format!("warn missing {}", skills_dir.display()));
    }
}

/// Report the timeout the client resolves, clamps included.
fn ai_timeout_secs(proxy: &mut dyn ShellProxy) -> u64 {
    crate::chatgpt::load_openai_config(proxy)
        .timeout()
        .as_secs()
}

fn check_ai(ctx: &Context, proxy: &mut dyn ShellProxy) {
    let api_key = proxy
        .get_var("AI_CHAT_API_KEY")
        .or_else(|| proxy.get_var("OPENAI_API_KEY"))
        .or_else(|| proxy.get_var("OPEN_AI_API_KEY"));
    let model = proxy
        .get_var("AI_CHAT_MODEL")
        .or_else(|| proxy.get_var("OPENAI_MODEL"))
        .unwrap_or_else(|| "gpt-5-mini".to_string());
    let base_url = proxy
        .get_var("AI_CHAT_BASE_URL")
        .or_else(|| proxy.get_var("OPENAI_BASE_URL"))
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let lang = proxy
        .get_var("AI_MESSAGE_LANG")
        .unwrap_or_else(|| "default".to_string());

    let key_state = if api_key
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "ok"
    } else {
        "warn"
    };
    let _ = ctx.write_stdout(&format!("{key_state} api-key {}", mask_secret(api_key)));
    let _ = ctx.write_stdout(&format!("ok model {model}"));
    let _ = ctx.write_stdout(&format!("ok base-url {base_url}"));
    let _ = ctx.write_stdout(&format!("ok message-lang {lang}"));

    let _ = ctx.write_stdout(&format!("ok request-timeout {}s", ai_timeout_secs(proxy)));

    match crate::chatgpt::chat_session_description() {
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

    let dsh_skills_dir = dirs::config_dir().map(|path| path.join("dsh").join("skills"));
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

    let codex_root = proxy
        .get_var("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|path| path.join(".codex")));
    let codex_skills_dir = codex_root.map(|path| path.join("skills"));
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

fn check_mcp(ctx: &Context, proxy: &mut dyn ShellProxy) {
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

    let state = if configured > 0 { "ok" } else { "warn" };
    let _ = ctx.write_stdout(&format!("{state} configured {configured}"));
    let _ = ctx.write_stdout(&format!("ok servers {servers}"));
    let _ = ctx.write_stdout(&format!("ok connected {connected}"));
    let _ = ctx.write_stdout(&format!("ok tools {tools}"));
    for server in configured_servers {
        if let McpTransport::Sse { url } = &server.transport {
            let _ = ctx.write_stdout(&format!(
                "warn mcp {} sse url={} configuration-only use-streamable-http",
                server.label, url
            ));
        }
    }
}

fn check_project(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let project = project_context::resolve_project_context(current_dir);

    let _ = ctx.write_stdout(&format!("ok cwd {}", current_dir.display()));
    let _ = ctx.write_stdout(&format!(
        "ok project-root {}",
        project.project_root.display()
    ));

    if project.project_markers.is_empty() {
        let _ = ctx.write_stdout("warn markers none");
    } else {
        let _ = ctx.write_stdout(&format!(
            "ok markers {}",
            project.project_markers.join(", ")
        ));
    }

    if project.runtimes.is_empty() {
        let _ = ctx.write_stdout("skip runtime none");
    } else {
        for runtime in project.runtimes {
            let version = runtime.version.unwrap_or_else(|| "-".to_string());
            let _ = ctx.write_stdout(&format!(
                "ok runtime {} source={} version={} path={}",
                runtime.name,
                runtime.source,
                version,
                runtime.path.display()
            ));
        }
    }

    if project.activations.is_empty() {
        let _ = ctx.write_stdout("skip activation none");
    } else {
        for activation in project.activations {
            let _ = ctx.write_stdout(&format!(
                "ok activation {} {}",
                activation.kind,
                activation.path.display()
            ));
        }
    }

    for line in proxy.completion_diagnostics() {
        let _ = ctx.write_stdout(&format!("ok {line}"));
    }
}

fn check_runtimes(ctx: &Context) {
    for command in [
        "mise", "direnv", "rustc", "cargo", "node", "npm", "pnpm", "python3", "uv", "go", "just",
    ] {
        match resolve_in_path(command) {
            Some(path) => {
                let version = read_version(command).unwrap_or_else(|| "-".to_string());
                let _ = ctx.write_stdout(&format!("ok {command} {version} {}", path.display()));
            }
            None => {
                let _ = ctx.write_stdout(&format!("warn {command} not-found"));
            }
        }
    }
}

fn check_performance(ctx: &Context, proxy: &mut dyn ShellProxy, args: &[String]) {
    let top_limit = performance_top_limit(args);
    match proxy.command_history_len() {
        Some(count) => {
            let _ = ctx.write_stdout(&format!("ok history-loaded entries={count}"));
        }
        None => {
            let _ = ctx.write_stdout("skip history-loaded unavailable");
        }
    }

    match proxy.executable_cache_len() {
        Some(count) => {
            let _ = ctx.write_stdout(&format!("ok path-cache memory-entries={count}"));
        }
        None => {
            let _ = ctx.write_stdout("skip path-cache memory-unavailable");
        }
    }

    match executable_cache_file_info() {
        Some((path, count)) => {
            let _ = ctx.write_stdout(&format!(
                "ok path-cache-file {} entries={count}",
                path.display()
            ));
        }
        None => {
            let _ = ctx.write_stdout("skip path-cache-file missing");
        }
    }

    let completion_diagnostics = proxy.completion_diagnostics();
    if completion_diagnostics.is_empty() {
        let _ = ctx.write_stdout("skip completion-cache unavailable");
    } else {
        for line in completion_diagnostics {
            let _ = ctx.write_stdout(&format!("ok {line}"));
        }
    }

    let _ = ctx.write_stdout("ok timing-flush debounce interval=5s threshold=10");

    if performance_latency_enabled(args) {
        let iterations = performance_latency_iterations(args).unwrap_or(1_000);
        let lines = proxy.latency_probe_lines(iterations);
        if lines.is_empty() {
            let _ = ctx.write_stdout("skip latency-probes unavailable");
        } else {
            for line in &lines {
                let _ = ctx.write_stdout(&format!("ok {line}"));
            }
            if let Some((name, avg_ns)) = slowest_latency_probe(&lines) {
                let _ = ctx.write_stdout(&format!(
                    "ok latency-slowest probe={name} avg={avg_ns}ns focus={}",
                    latency_probe_focus(name)
                ));
            }
        }
    } else {
        let _ = ctx.write_stdout("skip latency-probes pass --latency to run");
    }

    let timing_file = crate::command_timing::get_timing_file_path();
    match timing_file
        .as_ref()
        .and_then(crate::command_timing::CommandTiming::load_from_file)
    {
        Some(timing) => {
            let _ = ctx.write_stdout(&format!("ok timing-entries {}", timing.stats.len()));
            let _ = ctx.write_stdout(&format!("ok timing-top limit={top_limit}"));

            let slowest = timing.top_slowest(top_limit);
            if slowest.is_empty() {
                let _ = ctx.write_stdout("skip slowest none");
            } else {
                for (index, stats) in slowest.into_iter().enumerate() {
                    if index == 0 {
                        let _ = ctx.write_stdout(&format!(
                            "ok slowest {} avg={} success={:.1}%",
                            stats.command,
                            crate::command_timing::format_duration(stats.average_duration_ms()),
                            stats.success_rate()
                        ));
                    } else {
                        let _ = ctx.write_stdout(&format!(
                            "ok slowest#{} {} avg={} success={:.1}%",
                            index + 1,
                            stats.command,
                            crate::command_timing::format_duration(stats.average_duration_ms()),
                            stats.success_rate()
                        ));
                    }
                }
            }

            let frequent = timing.top_frequent(top_limit);
            if frequent.is_empty() {
                let _ = ctx.write_stdout("skip frequent none");
            } else {
                for (index, stats) in frequent.into_iter().enumerate() {
                    if index == 0 {
                        let _ = ctx.write_stdout(&format!(
                            "ok frequent {} calls={}",
                            stats.command, stats.total_calls
                        ));
                    } else {
                        let _ = ctx.write_stdout(&format!(
                            "ok frequent#{} {} calls={}",
                            index + 1,
                            stats.command,
                            stats.total_calls
                        ));
                    }
                }
            }
        }
        None => {
            let display_path = timing_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let _ = ctx.write_stdout(&format!("warn timing missing {}", display_path));
        }
    }

    let Some(config_root) = dirs::config_dir().map(|path| path.join("dsh")) else {
        let _ = ctx.write_stdout("warn skills-scan unable-to-determine-config-dir");
        return;
    };

    let skills_dir = config_root.join("skills");
    if skills_dir.exists() {
        let count = fs::read_dir(&skills_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        let _ = ctx.write_stdout(&format!(
            "ok skills-scan {} entries={count}",
            skills_dir.display()
        ));
    } else {
        let _ = ctx.write_stdout(&format!(
            "skip skills-scan missing {}",
            skills_dir.display()
        ));
    }
}

fn performance_latency_enabled(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--latency")
}

fn performance_latency_iterations(args: &[String]) -> Option<usize> {
    args.windows(2).find_map(|window| {
        if window[0] == "--latency-iters" {
            window[1].parse::<usize>().ok()
        } else {
            None
        }
    })
}

fn performance_top_limit(args: &[String]) -> usize {
    let parsed = args
        .windows(2)
        .find_map(|window| {
            if window[0] == "--top" {
                window[1].parse::<usize>().ok()
            } else {
                None
            }
        })
        .or_else(|| {
            args.iter().find_map(|arg| {
                arg.strip_prefix("--top=")
                    .and_then(|value| value.parse::<usize>().ok())
            })
        });

    parsed
        .filter(|value| *value > 0)
        .unwrap_or(PERFORMANCE_TOP_DEFAULT)
}

fn slowest_latency_probe(lines: &[String]) -> Option<(&str, u128)> {
    lines
        .iter()
        .filter_map(|line| latency_probe_name_and_avg(line))
        .max_by_key(|(_, avg_ns)| *avg_ns)
}

fn latency_probe_name_and_avg(line: &str) -> Option<(&str, u128)> {
    let rest = line.strip_prefix("latency ")?;
    let (name, metrics) = rest.split_once(' ')?;
    let avg_ns = metrics
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg=")?.strip_suffix("ns"))?
        .parse::<u128>()
        .ok()?;
    Some((name, avg_ns))
}

fn latency_probe_focus(name: &str) -> &'static str {
    if name.starts_with("integrated_completion") {
        "completion"
    } else if name.starts_with("repl_analyze") || name.starts_with("repl_print") {
        "repl"
    } else if name.starts_with("history") {
        "history"
    } else if name.contains("cache") {
        "cache"
    } else {
        "runtime"
    }
}

fn executable_cache_file_info() -> Option<(PathBuf, usize)> {
    let dirs = xdg::BaseDirectories::with_prefix("dsh");
    let path = dirs.place_data_file("executable_names.json").ok()?;
    let contents = fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let count = value
        .get("names")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Some((path, count))
}

const CODEX_CORE_SKILLS: &[&str] = &["doge-shell-repo"];
const DSH_COMMON_SKILLS: &[&str] = &[
    "doge-shell-repo",
    "doge-shell-validation",
    "doge-shell-investigation",
    "doge-shell-chat-tools",
];

fn check_skills(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let Some(repo_root) = find_repo_root(current_dir) else {
        let _ = ctx.write_stdout("warn repo-root not-found for skill diagnostics");
        return;
    };
    let source_root = repo_root.join("docs").join("ai").join("skills");
    if !source_root.is_dir() {
        let _ = ctx.write_stdout(&format!(
            "warn canonical-skills missing {}",
            source_root.display()
        ));
        return;
    }

    let canonical_count = count_skill_dirs(&source_root);
    let _ = ctx.write_stdout(&format!(
        "ok canonical-skills {} entries={canonical_count}",
        source_root.display()
    ));

    let codex_root = proxy
        .get_var("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|path| path.join(".codex")));
    if let Some(root) = codex_root {
        check_skill_profile(
            ctx,
            "codex",
            "codex-core",
            &source_root,
            &root.join("skills"),
            CODEX_CORE_SKILLS,
        );
    } else {
        let _ = ctx.write_stdout("warn codex-runtime-skills unable-to-determine-home-dir");
    }

    if let Some(root) = dirs::config_dir().map(|path| path.join("dsh").join("skills")) {
        check_skill_profile(
            ctx,
            "dsh",
            "dsh-common",
            &source_root,
            &root,
            DSH_COMMON_SKILLS,
        );
    } else {
        let _ = ctx.write_stdout("warn dsh-runtime-skills unable-to-determine-config-dir");
    }

    check_claude_project_skills(ctx, &repo_root, &source_root, canonical_count);
}

/// `<repo>/.claude/skills` is what Claude Code reads. It is normally a symlink
/// to the canonical source, so every skill is visible with nothing to sync.
fn check_claude_project_skills(
    ctx: &Context,
    repo_root: &Path,
    source_root: &Path,
    canonical_count: usize,
) {
    let dest_root = repo_root.join(".claude").join("skills");

    if !dest_root.exists() {
        let _ = ctx.write_stdout(&format!(
            "missing claude-project-skills {}",
            dest_root.display()
        ));
        return;
    }

    if dest_root.is_symlink() {
        match fs::canonicalize(&dest_root) {
            Ok(resolved) if fs::canonicalize(source_root).ok().as_deref() == Some(&resolved) => {
                let _ = ctx.write_stdout(&format!(
                    "ok claude-project-skills symlink -> docs/ai/skills entries={canonical_count}"
                ));
            }
            Ok(resolved) => {
                let _ = ctx.write_stdout(&format!(
                    "warn claude-project-skills symlink points at {}",
                    resolved.display()
                ));
            }
            Err(err) => {
                let _ =
                    ctx.write_stdout(&format!("warn claude-project-skills broken-symlink {err}"));
            }
        }
        return;
    }

    let installed = count_skill_dirs(&dest_root);
    let state = if installed == canonical_count {
        "ok"
    } else {
        "warn"
    };
    let _ = ctx.write_stdout(&format!(
        "{state} claude-project-skills copy entries={installed} canonical={canonical_count}"
    ));
}

fn check_skill_profile(
    ctx: &Context,
    target: &str,
    profile: &str,
    source_root: &Path,
    dest_root: &Path,
    expected_skills: &[&str],
) {
    let _ = ctx.write_stdout(&format!(
        "ok {target}-profile {profile} root={}",
        dest_root.display()
    ));

    let mut ok = 0;
    let mut stale = 0;
    let mut missing = 0;
    for skill in expected_skills {
        let source = source_root.join(skill);
        let dest = dest_root.join(skill);
        if !source.is_dir() {
            let _ = ctx.write_stdout(&format!("warn {target} {skill} source-missing"));
            continue;
        }
        if !dest.is_dir() {
            missing += 1;
            let _ = ctx.write_stdout(&format!("missing {target} {skill} -> {}", dest.display()));
        } else if skill_dirs_match(&source, &dest) {
            ok += 1;
            let _ = ctx.write_stdout(&format!("ok {target} {skill} -> {}", dest.display()));
        } else {
            stale += 1;
            let _ = ctx.write_stdout(&format!("stale {target} {skill} -> {}", dest.display()));
        }
    }

    let extra = count_extra_skill_dirs(dest_root, expected_skills);
    if extra > 0 {
        let _ = ctx.write_stdout(&format!(
            "warn {target}-runtime-skills extra entries={extra}"
        ));
    }
    let state = if stale == 0 && missing == 0 {
        "ok"
    } else {
        "warn"
    };
    let _ = ctx.write_stdout(&format!(
        "{state} {target}-runtime-skills summary ok={ok} stale={stale} missing={missing}"
    ));
}

fn check_safety(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let allowlist = proxy.list_execute_allowlist();
    if allowlist.is_empty() {
        let _ = ctx.write_stdout("ok execute-allowlist empty");
    } else {
        for entry in &allowlist {
            if is_risky_execute_allowlist_entry(entry) {
                let _ = ctx.write_stdout(&format!("warn execute-allowlist risky `{entry}`"));
            } else {
                let _ = ctx.write_stdout(&format!("ok execute-allowlist `{entry}`"));
            }
        }
    }

    let servers = proxy.list_mcp_servers();
    if servers.is_empty() {
        let _ = ctx.write_stdout("ok mcp-servers none");
    } else {
        for server in servers {
            match &server.transport {
                McpTransport::Stdio { command, env, .. } => {
                    if env.keys().any(|key| is_sensitive_env_name(key)) {
                        let _ = ctx.write_stdout(&format!(
                            "warn mcp {} stdio command={} sensitive-env",
                            server.label, command
                        ));
                    } else {
                        let _ = ctx.write_stdout(&format!(
                            "ok mcp {} stdio command={}",
                            server.label, command
                        ));
                    }
                }
                McpTransport::Sse { url } => {
                    let _ = ctx.write_stdout(&format!(
                        "warn mcp {} sse url={} configuration-only use-streamable-http",
                        server.label, url
                    ));
                }
                McpTransport::Http {
                    url, auth_header, ..
                } => {
                    let scheme = if is_https_or_local_http_url(url) {
                        "ok"
                    } else {
                        "warn"
                    };
                    let auth = if auth_header.is_some() { " auth" } else { "" };
                    let _ = ctx.write_stdout(&format!(
                        "{scheme} mcp {} http url={}{}",
                        server.label, url, auth
                    ));
                }
            }
        }
    }

    if let Some(base_url) = proxy.get_var("AI_CHAT_BASE_URL") {
        if is_https_or_local_http_url(&base_url) {
            let _ = ctx.write_stdout(&format!("ok ai-base-url {base_url}"));
        } else {
            let _ = ctx.write_stdout(&format!("warn ai-base-url insecure {base_url}"));
        }
    } else {
        let _ = ctx.write_stdout("ok ai-base-url default");
    }

    let project = project_context::resolve_project_context(current_dir);
    let envrc = project.project_root.join(".envrc");
    if envrc.exists() {
        if proxy.is_direnv_allowed(&project.project_root) {
            let _ = ctx.write_stdout(&format!("ok envrc allowed {}", envrc.display()));
        } else {
            let _ = ctx.write_stdout(&format!("warn envrc not-allowed {}", envrc.display()));
        }
    } else {
        let _ = ctx.write_stdout("ok envrc missing");
    }

    if let Some(config_root) = dirs::config_dir().map(|path| path.join("dsh").join("skills")) {
        if config_root.exists() {
            let count = fs::read_dir(&config_root)
                .map(|entries| entries.count())
                .unwrap_or(0);
            if count > 8 {
                let _ = ctx.write_stdout(&format!(
                    "warn runtime-skills footprint-high entries={count}"
                ));
            } else {
                let _ = ctx.write_stdout(&format!("ok runtime-skills entries={count}"));
            }
        } else {
            let _ = ctx.write_stdout("ok runtime-skills missing");
        }
    }

    if let Some(repo_root) = find_repo_root(current_dir) {
        match unignored_log_paths(&repo_root) {
            Ok(paths) if paths.is_empty() => {
                let _ = ctx.write_stdout("ok unignored-logs none");
            }
            Ok(paths) => {
                for path in paths {
                    let _ = ctx.write_stdout(&format!("warn unignored-log {}", path.display()));
                }
                let _ = ctx.write_stdout("warn unignored-logs consider adding *.log to .gitignore");
            }
            Err(err) => {
                let _ = ctx.write_stdout(&format!("warn unignored-logs unavailable {err}"));
            }
        }
    } else {
        let _ = ctx.write_stdout("skip unignored-logs repo-root-not-found");
    }
}

fn is_sensitive_env_name(key: &str) -> bool {
    safety_policy::is_sensitive_key(key)
}

fn is_https_or_local_http_url(value: &str) -> bool {
    if value.starts_with("https://") {
        return true;
    }

    let Some(rest) = value.strip_prefix("http://") else {
        return false;
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or_else(|| rest.split(['/', '?', '#']).next().unwrap_or_default());

    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };

    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn is_risky_execute_allowlist_entry(entry: &str) -> bool {
    let lower = entry.to_ascii_lowercase();
    lower.contains('|')
        || lower.contains(';')
        || lower.contains("$(")
        || lower.contains('`')
        || lower.contains("rm -rf")
        || lower.contains("rm -fr")
        || lower.starts_with("sh")
        || lower.starts_with("bash")
        || lower.starts_with("zsh")
        || lower.starts_with("python -c")
        || lower.starts_with("node -e")
}

fn unignored_log_paths(repo_root: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args(["status", "--short", "--untracked-files=all"])
        .current_dir(repo_root)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim().trim_matches('"');
            path.ends_with(".log").then(|| PathBuf::from(path))
        })
        .collect())
}

fn check_dev(ctx: &Context, current_dir: &Path) {
    let Some(repo_root) = find_repo_root(current_dir) else {
        let _ = ctx.write_stdout("warn repo-root not-found for validation suggestions");
        return;
    };
    let _ = ctx.write_stdout(&format!("ok repo-root {}", repo_root.display()));

    let changed = changed_paths(&repo_root);
    match changed {
        Ok(paths) if paths.is_empty() => {
            let _ = ctx.write_stdout("skip changed-files none");
        }
        Ok(paths) => {
            let _ = ctx.write_stdout(&format!("ok changed-files {}", paths.len()));
            for path in &paths {
                let _ = ctx.write_stdout(&format!("ok changed {}", path.display()));
            }
            let commands = validation_commands_for_paths(&paths);
            if commands.is_empty() {
                let _ = ctx.write_stdout("skip validation no focused command for changed files");
            } else {
                for command in commands {
                    let _ = ctx.write_stdout(&format!("ok validate {command}"));
                }
            }
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("warn changed-files unavailable {err}"));
        }
    }
}

fn find_repo_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    for ancestor in cwd.ancestors() {
        if ancestor.join("Cargo.toml").is_file() && ancestor.join("docs").join("ai").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn changed_paths(repo_root: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(repo_root)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(parse_git_status_short(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_git_status_short(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim();
            if path.is_empty() {
                return None;
            }
            let path = path
                .rsplit_once(" -> ")
                .map(|(_, new_path)| new_path)
                .unwrap_or(path);
            Some(PathBuf::from(path.trim_matches('"')))
        })
        .collect()
}

fn validation_commands_for_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut commands = Vec::new();
    let mut packages = BTreeSet::new();
    let mut needs_workspace_check = false;
    let mut needs_ai_guidance = false;
    let mut has_rust = false;

    for path in paths {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.ends_with(".rs") {
            has_rust = true;
        }
        if text == "Cargo.toml" || text == "Cargo.lock" {
            needs_workspace_check = true;
        }
        if text == "AGENTS.md"
            || text == "CLAUDE.md"
            || text.starts_with("docs/ai/")
            || text.starts_with(".claude/")
            || text == "scripts/install-runtime-skills.sh"
        {
            needs_ai_guidance = true;
        }

        // `completions/` is embedded into the `doge-shell` binary by rust-embed,
        // and `command-completion-schema.json` is asserted to match the provider
        // list in `dsh-types`.
        if text.starts_with("completions/") {
            packages.insert("doge-shell");
        }
        if text == "command-completion-schema.json" {
            packages.insert("doge-shell");
            packages.insert("dsh-types");
        }

        if text.starts_with("dsh-builtin/") {
            packages.insert("dsh-builtin");
        } else if text.starts_with("dsh-openai/") {
            packages.insert("dsh-openai");
        } else if text.starts_with("dsh-types/") {
            packages.insert("dsh-types");
        } else if text.starts_with("dsh-frecency/") {
            packages.insert("dsh-frecency");
        } else if text.starts_with("dsh/") {
            packages.insert("doge-shell");
        }
    }

    if has_rust {
        add_command(&mut commands, "cargo fmt --check");
    }
    for package in [
        "dsh-builtin",
        "doge-shell",
        "dsh-openai",
        "dsh-types",
        "dsh-frecency",
    ] {
        if packages.contains(package) {
            add_command(&mut commands, &format!("cargo test -p {package}"));
        }
    }
    if needs_workspace_check || packages.len() > 1 {
        add_command(&mut commands, "cargo check --workspace");
    }
    if packages.contains("doge-shell") {
        add_command(&mut commands, "cargo clippy -p doge-shell -- -D warnings");
    }
    if needs_ai_guidance {
        add_command(&mut commands, "scripts/check-ai-guidance.sh");
        add_command(&mut commands, "scripts/install-runtime-skills.sh --list");
        add_command(
            &mut commands,
            "scripts/install-runtime-skills.sh --status --target codex --profile codex-core",
        );
    }

    commands
}

fn add_command(commands: &mut Vec<String>, command: &str) {
    if !commands.iter().any(|existing| existing == command) {
        commands.push(command.to_string());
    }
}

fn count_extra_skill_dirs(root: &Path, expected_skills: &[&str]) -> usize {
    let expected = expected_skills.iter().copied().collect::<BTreeSet<_>>();
    fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    let path = entry.path();
                    path.is_dir()
                        && path.join("SKILL.md").is_file()
                        && entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| !expected.contains(name))
                })
                .count()
        })
        .unwrap_or(0)
}

fn skill_dirs_match(source: &Path, dest: &Path) -> bool {
    let Ok(source_files) = relative_files(source) else {
        return false;
    };
    let Ok(dest_files) = relative_files(dest) else {
        return false;
    };
    if source_files != dest_files {
        return false;
    }

    source_files.into_iter().all(|relative| {
        let source_path = source.join(&relative);
        let dest_path = dest.join(&relative);
        match (fs::read(source_path), fs::read(dest_path)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    })
}

fn relative_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files)?;
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                files.push(relative.to_path_buf());
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn mask_secret(value: Option<String>) -> String {
    match value {
        Some(secret) if !secret.is_empty() => {
            let visible = secret.chars().rev().take(4).collect::<String>();
            let suffix = visible.chars().rev().collect::<String>();
            format!("***{}", suffix)
        }
        _ => "missing".to_string(),
    }
}

fn read_version(command: &str) -> Option<String> {
    let args = match command {
        "go" => vec!["version"],
        _ => vec!["--version"],
    };
    let output = Command::new(command).args(args).output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout
        .lines()
        .chain(stderr.lines())
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

fn resolve_in_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            return metadata.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn count_skill_dirs(root: &Path) -> usize {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    let path = entry.path();
                    path.is_dir() && path.join("SKILL.md").is_file()
                })
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::mcp::{McpServerConfig, McpTransport};
    use dsh_types::observed_output::{ObservedOutput, SharedOutputObserver};
    use std::collections::HashMap;
    use std::os::fd::IntoRawFd;
    use std::process::Command as StdCommand;

    struct TestProxy {
        cwd: PathBuf,
        vars: HashMap<String, String>,
        allowlist: Vec<String>,
        servers: Vec<McpServerConfig>,
        direnv_allowed: bool,
    }

    impl ShellProxy for TestProxy {
        fn exit_shell(&mut self) {}

        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }

        fn get_git_branch(&self) -> Option<String> {
            None
        }

        fn get_job_count(&self) -> usize {
            0
        }

        fn dispatch(
            &mut self,
            _ctx: &Context,
            _cmd: &str,
            _argv: Vec<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn save_path_history(&mut self, _path: &str) {}

        fn changepwd(&mut self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn insert_path(&mut self, _index: usize, _path: &str) {}

        fn get_var(&mut self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }

        fn set_var(&mut self, _key: String, _value: String) {}

        fn set_env_var(&mut self, _key: String, _value: String) {}

        fn is_direnv_allowed(&self, _path: &Path) -> bool {
            self.direnv_allowed
        }

        fn unset_env_var(&mut self, _key: &str) {}

        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }

        fn set_alias(&mut self, _name: String, _command: String) {}

        fn list_aliases(&mut self) -> HashMap<String, String> {
            HashMap::new()
        }

        fn add_abbr(&mut self, _name: String, _expansion: String) {}

        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }

        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }

        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }

        fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
            self.servers.clone()
        }

        fn list_execute_allowlist(&mut self) -> Vec<String> {
            self.allowlist.clone()
        }

        fn list_exported_vars(&self) -> Vec<(String, String)> {
            Vec::new()
        }

        fn export_var(&mut self, _key: &str) -> bool {
            false
        }

        fn set_and_export_var(&mut self, _key: String, _value: String) {}

        fn get_current_dir(&self) -> anyhow::Result<PathBuf> {
            Ok(self.cwd.clone())
        }

        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
    }

    fn observed_context() -> (Context, SharedOutputObserver) {
        let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let observer = ObservedOutput::shared(8192);
        ctx.output_observer = Some(observer.clone());
        ctx.outfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        ctx.errfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
        (ctx, observer)
    }

    fn observed_stdout(observer: &SharedOutputObserver) -> String {
        observer.lock().unwrap().snapshot().stdout
    }

    #[test]
    fn mask_secret_hides_prefix() {
        assert_eq!(mask_secret(Some("abcdef".to_string())), "***cdef");
        assert_eq!(mask_secret(None), "missing");
    }

    #[test]
    fn show_section_matches_alias() {
        assert!(show_section(Some("runtime"), "runtimes"));
        assert!(show_section(None, "ai"));
        assert!(!show_section(Some("ai"), "mcp"));
    }

    #[test]
    fn help_text_lists_sections_and_examples() {
        let help = help_text();
        assert!(help.contains("Usage: doctor"));
        assert!(help.contains("config"));
        assert!(help.contains("ai"));
        assert!(help.contains("mcp"));
        assert!(help.contains("project"));
        assert!(help.contains("runtime"));
        assert!(help.contains("performance"));
        assert!(help.contains("--latency"));
        assert!(help.contains("skills"));
        assert!(help.contains("safety"));
        assert!(help.contains("setup"));
        assert!(help.contains("fix"));
        assert!(help.contains("validate"));
        assert!(help.contains("doctor ai"));
        assert!(help.contains("doctor setup"));
    }

    #[test]
    fn performance_latency_options_are_detected() {
        let args = vec![
            "--latency".to_string(),
            "--latency-iters".to_string(),
            "250".to_string(),
        ];
        assert!(performance_latency_enabled(&args));
        assert_eq!(performance_latency_iterations(&args), Some(250));
        assert!(!performance_latency_enabled(&[]));
        assert_eq!(performance_latency_iterations(&[]), None);
    }

    #[test]
    fn performance_top_option_defaults_and_parses() {
        assert_eq!(performance_top_limit(&[]), PERFORMANCE_TOP_DEFAULT);
        assert_eq!(
            performance_top_limit(&["--top".to_string(), "3".to_string()]),
            3
        );
        assert_eq!(performance_top_limit(&["--top=7".to_string()]), 7);
        assert_eq!(
            performance_top_limit(&["--top".to_string(), "0".to_string()]),
            PERFORMANCE_TOP_DEFAULT
        );
        assert_eq!(
            performance_top_limit(&["--top".to_string(), "bad".to_string()]),
            PERFORMANCE_TOP_DEFAULT
        );
    }

    #[test]
    fn latency_probe_summary_selects_slowest_focus() {
        let lines = vec![
            "latency completion_cache_lookup total=10us avg=10ns iterations=1".to_string(),
            "latency integrated_completion_git_subcommand_warm total=100us avg=100ns iterations=1"
                .to_string(),
            "latency repl_analyze_input total=50us avg=50ns iterations=1".to_string(),
        ];

        let (name, avg_ns) = slowest_latency_probe(&lines).expect("slowest probe");
        assert_eq!(name, "integrated_completion_git_subcommand_warm");
        assert_eq!(avg_ns, 100);
        assert_eq!(latency_probe_focus(name), "completion");
    }

    #[test]
    fn show_section_matches_new_aliases() {
        assert!(show_section(Some("validate"), "dev"));
        assert!(is_known_section("skills"));
        assert!(is_known_section("safety"));
        assert!(is_known_section("setup"));
        assert!(is_known_section("fix"));
        assert!(!is_known_section("unknown"));
    }

    #[test]
    fn default_config_lisp_contains_safe_setup_defaults() {
        let config = default_config_lisp();
        assert!(config.contains("chat-execute-clear"));
        assert!(config.contains("chat-execute-add"));
        assert!(config.contains("allow-direnv"));
    }

    #[test]
    fn project_section_uses_resolved_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mise.toml"), "[tools]\nnode = '20.11.0'\n").unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();

        let project = project_context::resolve_project_context(dir.path());
        let expected_root = std::fs::canonicalize(dir.path()).unwrap();
        let actual_root = std::fs::canonicalize(&project.project_root).unwrap();
        assert_eq!(actual_root, expected_root);
        assert!(
            project
                .project_markers
                .iter()
                .any(|marker| marker == "mise.toml")
        );
        assert!(
            project
                .runtimes
                .iter()
                .any(|runtime| runtime.name == "node" && runtime.source == "mise")
        );
    }

    #[test]
    fn count_skill_dirs_only_counts_skill_folders() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("doge-shell-repo");
        let plain_dir = dir.path().join("notes");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::create_dir_all(&plain_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# skill").unwrap();
        std::fs::write(plain_dir.join("README.md"), "# note").unwrap();

        assert_eq!(count_skill_dirs(dir.path()), 1);
    }

    #[test]
    fn skill_dirs_match_detects_stale_runtime_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(source.join("SKILL.md"), "# skill\n").unwrap();
        std::fs::write(dest.join("SKILL.md"), "# skill\n").unwrap();

        assert!(skill_dirs_match(&source, &dest));

        std::fs::write(dest.join("SKILL.md"), "# stale\n").unwrap();
        assert!(!skill_dirs_match(&source, &dest));
    }

    #[test]
    fn parse_git_status_short_handles_renames() {
        let paths = parse_git_status_short(
            " M dsh-builtin/src/task.rs\nR  old/path.rs -> dsh/src/new_path.rs\n?? docs/ai/new.md\n",
        );
        assert_eq!(paths[0], PathBuf::from("dsh-builtin/src/task.rs"));
        assert_eq!(paths[1], PathBuf::from("dsh/src/new_path.rs"));
        assert_eq!(paths[2], PathBuf::from("docs/ai/new.md"));
    }

    #[test]
    fn validation_commands_follow_changed_paths() {
        let paths = vec![
            PathBuf::from("dsh-builtin/src/task.rs"),
            PathBuf::from("dsh/src/lib.rs"),
            PathBuf::from("docs/ai/README.md"),
        ];
        let commands = validation_commands_for_paths(&paths);

        assert!(commands.iter().any(|cmd| cmd == "cargo fmt --check"));
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "cargo test -p dsh-builtin")
        );
        assert!(commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"));
        assert!(commands.iter().any(|cmd| cmd == "cargo check --workspace"));
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "scripts/check-ai-guidance.sh")
        );
        assert!(commands.iter().any(|cmd| {
            cmd == "scripts/install-runtime-skills.sh --status --target codex --profile codex-core"
        }));
    }

    #[test]
    fn completion_definitions_map_to_the_embedding_package() {
        let commands = validation_commands_for_paths(&[PathBuf::from("completions/git.json")]);
        assert!(
            commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"),
            "editing completions/ must still propose the doge-shell tests: {commands:?}"
        );

        let commands =
            validation_commands_for_paths(&[PathBuf::from("command-completion-schema.json")]);
        assert!(commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"));
        assert!(commands.iter().any(|cmd| cmd == "cargo test -p dsh-types"));
    }

    #[test]
    fn claude_guidance_paths_request_the_guidance_check() {
        let commands = validation_commands_for_paths(&[PathBuf::from(".claude/settings.json")]);
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "scripts/check-ai-guidance.sh"),
            "{commands:?}"
        );
    }

    #[test]
    fn safety_helpers_flag_risky_allowlist_entries() {
        assert!(is_risky_execute_allowlist_entry("bash"));
        assert!(is_risky_execute_allowlist_entry("python -c"));
        assert!(is_risky_execute_allowlist_entry("rm -rf /tmp/demo"));
        assert!(!is_risky_execute_allowlist_entry("git status"));
        assert!(is_sensitive_env_name("API_TOKEN"));
        assert!(is_sensitive_env_name("PRIVATE_KEY"));
        assert!(is_sensitive_env_name("GOOGLE_APPLICATION_CREDENTIALS"));
        assert!(!is_sensitive_env_name("PATH"));
    }

    #[test]
    fn safety_url_helper_rejects_localhost_prefix_spoofing() {
        assert!(is_https_or_local_http_url("https://api.example.com/v1"));
        assert!(is_https_or_local_http_url("http://localhost:8080/v1"));
        assert!(is_https_or_local_http_url("http://127.0.0.1/v1"));
        assert!(is_https_or_local_http_url("http://[::1]:8080/v1"));
        assert!(!is_https_or_local_http_url("http://example.com/v1"));
        assert!(!is_https_or_local_http_url("http://localhost.evil.com/v1"));
        assert!(!is_https_or_local_http_url("http://127.0.0.1.evil.com/v1"));
    }

    #[test]
    fn doctor_safety_reports_risky_posture() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("docs/ai")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "!debug.log\n").unwrap();
        std::fs::write(dir.path().join(".envrc"), "export FOO=bar\n").unwrap();
        std::fs::write(dir.path().join("debug.log"), "debug\n").unwrap();
        let git_init = StdCommand::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(git_init.status.success());

        let mut mcp_env = HashMap::new();
        mcp_env.insert("API_TOKEN".to_string(), "secret".to_string());
        let mut vars = HashMap::new();
        vars.insert(
            "AI_CHAT_BASE_URL".to_string(),
            "http://example.com/v1".to_string(),
        );
        let mut proxy = TestProxy {
            cwd: dir.path().to_path_buf(),
            vars,
            allowlist: vec!["bash".to_string(), "git status".to_string()],
            servers: vec![
                McpServerConfig {
                    label: "local".to_string(),
                    description: None,
                    transport: McpTransport::Stdio {
                        command: "node".to_string(),
                        args: Vec::new(),
                        env: mcp_env,
                        cwd: None,
                    },
                },
                McpServerConfig {
                    label: "legacy".to_string(),
                    description: None,
                    transport: McpTransport::Sse {
                        url: "https://example.com/sse".to_string(),
                    },
                },
            ],
            direnv_allowed: false,
        };
        let (ctx, observer) = observed_context();

        let status = command(
            &ctx,
            vec!["doctor".to_string(), "safety".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        let output = observed_stdout(&observer);
        assert!(output.contains("[safety]"));
        assert!(output.contains("warn execute-allowlist risky `bash`"));
        assert!(output.contains("ok execute-allowlist `git status`"));
        assert!(output.contains("warn mcp local stdio command=node sensitive-env"));
        assert!(output.contains(
            "warn mcp legacy sse url=https://example.com/sse configuration-only use-streamable-http"
        ));
        assert!(output.contains("warn ai-base-url insecure http://example.com/v1"));
        assert!(output.contains("warn envrc not-allowed"));
        assert!(output.contains("warn unignored-log debug.log"));

        let (ctx, observer) = observed_context();
        let status = command(
            &ctx,
            vec![
                "doctor".to_string(),
                "safety".to_string(),
                "--json".to_string(),
            ],
            &mut proxy,
        );
        assert_eq!(status, ExitStatus::ExitedWith(0));
        let value: serde_json::Value =
            serde_json::from_str(observed_stdout(&observer).trim()).unwrap();
        assert_eq!(value["section"], "safety");
        assert_eq!(value["details"]["execute_allowlist"][0]["risky"], true);
        assert_eq!(value["details"]["mcp"]["sse_servers"], 1);
        assert_eq!(value["details"]["envrc"]["allowed"], false);
    }

    #[test]
    fn validate_json_contains_focused_commands() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("docs/ai")).unwrap();
        std::fs::create_dir_all(dir.path().join("dsh/src")).unwrap();
        assert!(
            StdCommand::new("git")
                .arg("init")
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success()
        );
        std::fs::write(dir.path().join("dsh/src/review.rs"), "// changed\n").unwrap();

        let value = json_dev_details(dir.path());
        let commands = value["commands"].as_array().unwrap();
        assert!(
            commands
                .iter()
                .any(|command| command == "cargo test -p doge-shell")
        );
        assert!(
            value["changed_files"]
                .as_array()
                .is_some_and(|files| !files.is_empty())
        );
    }

    #[test]
    fn doctor_mcp_reports_legacy_sse_as_configuration_only() {
        let mut proxy = TestProxy {
            cwd: PathBuf::from("."),
            vars: HashMap::new(),
            allowlist: Vec::new(),
            servers: vec![McpServerConfig {
                label: "legacy".to_string(),
                description: None,
                transport: McpTransport::Sse {
                    url: "https://example.com/sse".to_string(),
                },
            }],
            direnv_allowed: false,
        };
        let (ctx, observer) = observed_context();

        let status = command(
            &ctx,
            vec!["doctor".to_string(), "mcp".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        let output = observed_stdout(&observer);
        assert!(output.contains("[mcp]"));
        assert!(output.contains("ok configured 1"));
        assert!(output.contains(
            "warn mcp legacy sse url=https://example.com/sse configuration-only use-streamable-http"
        ));
    }
}
