use crate::ShellProxy;
use crate::chatgpt::skills::lint::{self, LintLevel};
use crate::chatgpt::skills::usage;
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
    if show_section(section, "hooks") {
        print_header(ctx, "hooks");
        check_hooks(ctx, proxy);
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

/// Where the Codex runtime skills live.
///
/// `CODEX_HOME` moves the whole Codex directory, so the `--json` path used to
/// report a directory nobody was using once it was set.
fn codex_runtime_skills_dir(proxy: &mut dyn ShellProxy) -> Option<PathBuf> {
    proxy
        .get_var("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|path| path.join(".codex")))
        .map(|path| path.join("skills"))
}

fn json_section_details(
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
        "Usage: doctor [config|ai|hooks|mcp|project|runtime|performance|skills|safety|setup|fix|dev|validate] [OPTIONS]\n",
        "\n",
        "Run diagnostics for the current shell setup. Without a section, all checks run.\n",
        "\n",
        "Sections:\n",
        "  config   Check config.lisp and runtime skills directory\n",
        "  ai       Check AI-related environment and defaults\n",
        "  hooks    Inspect AI chat hook configuration without running any hook\n",
        "  mcp      Check configured MCP servers and connection counters\n",
        "  project  Detect project marker files in the current directory\n",
        "  runtime  Check common developer tools in PATH\n",
        "  performance  Show command timing and runtime skill scan state\n",
        "  skills   Show loaded skills and compare repo-local skills with runtime skills\n",
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
        "  doctor hooks\n",
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
            | "hooks"
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
    // `doctor fix` creates files here, so this must be the directory the shell
    // actually loads from - `dirs::config_dir()` on macOS is not.
    let config_root = crate::config_paths::config_home();

    ensure_setup_dir(ctx, &config_root, "config-root", fix);
    ensure_setup_dir(
        ctx,
        &crate::config_paths::skills_dir(),
        "runtime-skills",
        fix,
    );
    ensure_setup_dir(ctx, &config_root.join("completions"), "completion-dir", fix);
    ensure_setup_dir(
        ctx,
        &config_root.join("output-schemas"),
        "output-schema-dir",
        fix,
    );
    ensure_config_file(ctx, &crate::config_paths::config_file("config.lisp"), fix);

    let config = crate::chatgpt::load_openai_config(proxy);
    if let Some(api_key) = config.api_key() {
        let _ = ctx.write_stdout(&format!(
            "ok ai-key {}",
            mask_secret(Some(api_key.to_string()))
        ));
    } else {
        let _ = ctx.write_stdout(&format!(
            "warn ai-key missing {}",
            dsh_openai::API_KEY_SETUP_HINT
        ));
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
    let config_path = crate::config_paths::config_file("config.lisp");
    if config_path.exists() {
        let _ = ctx.write_stdout(&format!("ok config {}", config_path.display()));
    } else {
        let _ = ctx.write_stdout(&format!("warn missing {}", config_path.display()));
    }

    let skills_dir = crate::config_paths::skills_dir();
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

/// Mirrors `dsh/src/agent_lifecycle/herdr.rs::non_empty_env` exactly (trims,
/// then treats an all-whitespace value as unset) so this diagnostic and the
/// real detection it describes can never disagree about what counts as
/// "set".
fn non_empty_env_for_doctor(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn check_runtimes(ctx: &Context) {
    for command in [
        "mise", "direnv", "rustc", "cargo", "node", "npm", "pnpm", "python3", "uv", "go", "just",
        "herdr",
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

    // Pure process-environment reads, mirroring exactly what
    // `dsh/src/agent_lifecycle/herdr.rs::HerdrEnv::detect` requires - these
    // are ambient launch-time facts, not dsh settings, so this deliberately
    // doesn't go through `ShellProxy`/`resolve_setting`. `dsh-builtin`
    // cannot see `dsh`'s own activation state directly (the dependency runs
    // the other way), so replicating the same three checks here is the only
    // way to avoid reporting "active" when this process's own lifecycle
    // manager would in fact be a no-op `NullReporter`.
    let herdr_env = std::env::var("HERDR_ENV").ok();
    let pane_id = non_empty_env_for_doctor("HERDR_PANE_ID");
    let bin_path = non_empty_env_for_doctor("HERDR_BIN_PATH");
    let nested_owner = std::env::var_os("DSH_HERDR_OWNER_PID").is_some();
    match (herdr_env.as_deref(), pane_id, bin_path, nested_owner) {
        (Some("1"), Some(pane_id), Some(_), false) => {
            let _ = ctx.write_stdout(&format!("ok herdr-pane active pane={pane_id}"));
        }
        (Some("1"), Some(pane_id), _, true) => {
            let _ = ctx.write_stdout(&format!(
                "skip herdr-pane pane={pane_id} but an ancestor dsh already owns lifecycle authority for it"
            ));
        }
        (Some("1"), Some(_), None, false) => {
            let _ = ctx.write_stdout(
                "warn herdr-pane HERDR_ENV set but HERDR_BIN_PATH is missing or empty",
            );
        }
        _ => {
            let _ = ctx.write_stdout("skip herdr-pane not running under herdr");
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

    let skills_dir = crate::config_paths::skills_dir();
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
    // What the chat runtime will actually load, reported first: the drift check
    // below needs this repository, and most shells are not in it.
    report_runtime_skills(ctx, proxy, current_dir);

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

    if let Some(root) = codex_runtime_skills_dir(proxy) {
        check_skill_profile(
            ctx,
            "codex",
            "codex-core",
            &source_root,
            &root,
            CODEX_CORE_SKILLS,
        );
    } else {
        let _ = ctx.write_stdout("warn codex-runtime-skills unable-to-determine-home-dir");
    }

    check_skill_profile(
        ctx,
        "dsh",
        "dsh-common",
        &source_root,
        &crate::config_paths::skills_dir(),
        DSH_COMMON_SKILLS,
    );

    check_claude_project_skills(ctx, &repo_root, &source_root, canonical_count);
}

/// What `ai-hooks.json` says, without running a single hook.
///
/// A diagnostic that executes the user's configured commands would be a
/// surprise, so this stops at "does the program exist".
fn check_hooks(ctx: &Context, proxy: &mut dyn ShellProxy) {
    use crate::chatgpt::hooks::config;

    if config::nested_in_a_hook() {
        let _ = ctx.write_stdout("skip depth this shell runs inside a hook, so hooks are off");
        return;
    }
    if !config::enabled(proxy) {
        // Say so before listing anything: a report that shows configured hooks
        // while none of them can fire reads as "these are running".
        let _ = ctx.write_stdout(&format!(
            "skip enabled {}=off, so no hook runs in this shell",
            config::HOOKS_ENABLED_KEY
        ));
        return;
    }

    let Some(path) = config::config_path(proxy) else {
        let _ = ctx.write_stdout(&format!(
            "skip config none at {}",
            crate::config_paths::config_home()
                .join(config::HOOKS_CONFIG_FILE)
                .display()
        ));
        return;
    };
    if !path.is_file() {
        let _ = ctx.write_stdout(&format!("skip config none at {}", path.display()));
        return;
    }

    let hooks = match config::read(&path) {
        Ok(hooks) => hooks,
        Err(err) => {
            let _ = ctx.write_stdout(&format!("error config {}: {err}", path.display()));
            return;
        }
    };

    let _ = ctx.write_stdout(&format!("ok config {}", path.display()));
    if hooks.is_empty() {
        let _ = ctx.write_stdout("ok hooks 0");
        return;
    }

    for hook in hooks.all() {
        let events = hook
            .events
            .iter()
            .map(|event| event.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let state = if hook.enabled { "ok" } else { "skip" };
        let _ = ctx.write_stdout(&format!(
            "{state} hook {} events={events} {} timeout={}ms",
            hook.id,
            describe_matcher(hook),
            hook.timeout_ms()
        ));

        if !program_is_runnable(&hook.command[0]) {
            let _ = ctx.write_stdout(&format!(
                "warn hook {} command not found: {}",
                hook.id, hook.command[0]
            ));
        }

        // Said rather than refused. Listing `session-start` next to
        // `pre-tool-use` is reasonable to write, and rejecting it would break
        // configurations that work; a hook silently never firing on one of its
        // events - or never narrowing at all - is what the person needs told.
        if hook.matcher_narrows_nothing() {
            let _ = ctx.write_stdout(&format!(
                "warn hook {} match narrows nothing; it runs on every call",
                hook.id
            ));
        }
        let kinds = hook.argument_matcher_kinds();
        if !kinds.is_empty() {
            for event in hook.events.iter().filter(|event| !event.carries_a_tool()) {
                let _ = ctx.write_stdout(&format!(
                    "warn hook {} match ({}) cannot be satisfied on {}",
                    hook.id,
                    kinds.join(","),
                    event.as_str()
                ));
            }
        }
    }
}

/// The `match` clause as one field per kind, so `doctor` shows what narrowed a
/// hook and not only that something did.
fn describe_matcher(hook: &crate::chatgpt::hooks::config::HookDefinition) -> String {
    let Some(matcher) = hook.matcher.as_ref() else {
        return "tools=*".to_string();
    };
    let mut parts = Vec::new();
    let mut push = |name: &str, values: &[String]| {
        if !values.is_empty() {
            parts.push(format!("{name}={}", values.join(",")));
        }
    };
    push("tools", &matcher.tools);
    push("programs", &matcher.programs);
    push("paths", &matcher.paths);
    if !matcher.arguments.is_empty() {
        parts.push(format!(
            "args={}",
            matcher
                .arguments
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if parts.is_empty() {
        parts.push("tools=*".to_string());
    }
    parts.join(" ")
}

/// Can this program be started at all? Existence only - never execution.
fn program_is_runnable(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() || program.contains('/') {
        return path.is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())
}

fn json_hooks_details(proxy: &mut dyn ShellProxy) -> serde_json::Value {
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

/// The skills the `!` runtime would load here, and what they have cost.
///
/// Every summary is in the system prompt on every turn, so a skill nobody reads
/// is a recurring bill rather than a dormant file.
///
/// Reports the project root even when `AI_CHAT_PROJECT_SKILLS` is off, but says
/// so: claiming the runtime loads skills it will never see is worse than not
/// mentioning them.
fn report_runtime_skills(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let project_enabled = crate::chatgpt::resolve_project_skills_enabled(proxy);
    let manager = crate::chatgpt::skills::SkillsManager::new(Some(current_dir), true);
    let roots: Vec<crate::chatgpt::skills::SkillRoot> = manager.roots().to_vec();

    for root in &roots {
        let path = &root.path;
        let is_project = root.scope == crate::chatgpt::skills::SkillScope::Project;
        // Per root, not per scope: a project has two, and one label for both
        // would report the shared directory's entry count as the repository's.
        let label = format!("{}-skills", root.label());
        if !path.exists() {
            let _ = ctx.write_stdout(&format!("skip {label} missing {}", path.display()));
        } else if !path.is_dir() {
            // Distinct from "missing": saying missing sends the user looking in
            // the wrong place.
            let _ = ctx.write_stdout(&format!("error {label} not-a-directory {}", path.display()));
        } else if is_project && !project_enabled {
            let _ = ctx.write_stdout(&format!(
                "skip {label} {} entries={} AI_CHAT_PROJECT_SKILLS=off",
                path.display(),
                count_skill_dirs(path)
            ));
        } else {
            let _ = ctx.write_stdout(&format!(
                "ok {label} {} entries={}",
                path.display(),
                count_skill_dirs(path)
            ));
        }
    }

    // The trust decision is what actually decides whether these reach a prompt.
    // One line per root: each is agreed to separately, so one summary would
    // report a repository as trusted while a second directory was not.
    for decision in crate::chatgpt::skills::describe_project_roots(manager.roots()) {
        let trusted =
            crate::chatgpt::skills::trust::is_remembered(&decision.root, &decision.digest);
        let _ = ctx.write_stdout(&format!(
            "{} project-skills-trust {} {} skills={}",
            if trusted { "ok" } else { "warn" },
            if trusted {
                "remembered"
            } else {
                "not-yet-agreed"
            },
            crate::config_paths::display_path(&decision.root),
            decision.names.len()
        ));
    }

    let (skills, problems) = manager.load_reporting();
    for problem in &problems {
        // The root's label, not the scope's: two project roots are trusted
        // separately and edited separately, so one word for both is the
        // ambiguity `label()` was added to remove.
        let label = roots
            .iter()
            .find(|root| problem.path.starts_with(&root.path))
            .map(|root| root.label())
            .unwrap_or_else(|| problem.scope.as_str());
        let _ = ctx.write_stdout(&format!(
            "warn {label}-skill {} {}",
            problem.path.display(),
            problem.problem
        ));
    }
    // The deep pass: read what each loaded skill actually holds. Cheap enough
    // for a `doctor` run (which reads files anyway); too slow to run on every
    // turn, which is why `chat_with_tools` never calls this.
    for skill in &skills {
        let label = roots
            .iter()
            .find(|root| skill.dir().starts_with(&root.path))
            .map(|root| root.label())
            .unwrap_or_else(|| skill.scope.as_str());
        for finding in lint::lint_path(skill.dir(), &skill.name) {
            let severity = match finding.level {
                // A rejection here means `skill_manage` would have refused
                // this exact content; it reached disk some other way (a
                // human edit, or a skill this shell did not write).
                LintLevel::Reject => "error",
                LintLevel::Warn => "warn",
            };
            let _ = ctx.write_stdout(&format!(
                "{severity} {label}-skill {} {}",
                crate::config_paths::display_path(skill.dir()),
                finding.message
            ));
        }
    }

    let records = usage::load();
    let now = usage::now_ms();

    let mut authored = 0usize;
    let mut unused = Vec::new();
    let mut archived = 0usize;
    let mut pinned = 0usize;
    for skill in &skills {
        let record = records.get(&usage::key(skill.dir()));
        if record.is_some_and(|r| r.created_by == "agent") {
            authored += 1;
        }
        let is_archived = usage::is_archived(record);
        if is_archived {
            archived += 1;
        }
        if record.is_some_and(|r| r.pinned) {
            pinned += 1;
        }
        // The same rule `skill list` uses. Two copies disagreed at the boundary,
        // so one command called a skill dead while the other called it healthy.
        // Archived, not unread: it is already out of the prompt on purpose.
        if !is_archived && usage::is_stale(record, now) {
            unused.push(skill.name.clone());
        }
    }

    let _ = ctx.write_stdout(&format!("ok ai-authored-skills {authored}"));
    if unused.is_empty() {
        let _ = ctx.write_stdout("ok unused-skills 0");
    } else {
        let _ = ctx.write_stdout(&format!(
            "warn unused-skills {} not read recently: {}",
            unused.len(),
            unused.join(",")
        ));
    }
    let _ = ctx.write_stdout(&format!("ok archived-skills {archived}"));
    let _ = ctx.write_stdout(&format!("ok pinned-skills {pinned}"));

    let (proposals, broken) = crate::chatgpt::skills::pending::list();
    if proposals.is_empty() {
        // Still printed even when only `broken` is non-empty: a consumer
        // scanning for this line by name must always find one.
        let _ = ctx.write_stdout("ok pending-skills 0");
    } else {
        let names: Vec<&str> = proposals.iter().map(|p| p.name.as_str()).collect();
        let _ = ctx.write_stdout(&format!(
            "warn pending-skills {} awaiting review: {}",
            proposals.len(),
            names.join(",")
        ));
    }
    if !broken.is_empty() {
        let _ = ctx.write_stdout(&format!(
            "warn pending-skills-unreadable {} {}",
            broken.len(),
            broken
                .iter()
                .map(|b| crate::config_paths::display_path(&b.path))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
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

    {
        // Counted from `skill_roots`, not from a hand-picked pair. Naming the
        // roots here meant `.agents/skills` contributed nothing, and a
        // repository with forty skills there still reported a minimal
        // footprint - the very thing this check was added to catch.
        let roots = crate::chatgpt::skills::skill_roots(Some(current_dir), true);
        let entries = |scope| {
            roots
                .iter()
                .filter(|root| root.scope == scope && root.path.is_dir())
                .map(|root| count_skill_dirs(&root.path))
                .sum::<usize>()
        };
        let personal = entries(crate::chatgpt::skills::SkillScope::User);
        let project = entries(crate::chatgpt::skills::SkillScope::Project);
        let count = personal + project;
        if count > 8 {
            let _ = ctx.write_stdout(&format!(
                "warn runtime-skills footprint-high entries={count} personal={personal} project={project}"
            ));
        } else {
            let _ = ctx.write_stdout(&format!(
                "ok runtime-skills entries={count} personal={personal} project={project}"
            ));
        }

        // The two surfaces this shell gained: a cloned repository's skills, and
        // hooks that run external commands. Neither appeared in the safety
        // posture, which is where a person looks before trusting a checkout.
        let decisions = crate::chatgpt::skills::describe_project_roots(
            &crate::chatgpt::skills::skill_roots(Some(current_dir), true),
        );
        if decisions.is_empty() {
            let _ = ctx.write_stdout("ok project-skills none");
        }
        for decision in decisions {
            let trusted =
                crate::chatgpt::skills::trust::is_remembered(&decision.root, &decision.digest);
            let _ = ctx.write_stdout(&format!(
                "{} project-skills {} {} skills={}",
                if trusted { "warn" } else { "ok" },
                if trusted { "trusted" } else { "not-yet-agreed" },
                decision.root.display(),
                decision.names.len()
            ));
        }
    }

    {
        use crate::chatgpt::hooks::config as hooks_config;
        if !hooks_config::enabled(proxy) {
            let _ = ctx.write_stdout("ok ai-hooks off");
        } else {
            match hooks_config::config_path(proxy).filter(|path| path.is_file()) {
                None => {
                    let _ = ctx.write_stdout("ok ai-hooks none");
                }
                Some(path) => match hooks_config::read(&path) {
                    Ok(hooks) if hooks.is_empty() => {
                        let _ = ctx.write_stdout(&format!("ok ai-hooks none {}", path.display()));
                    }
                    Ok(hooks) => {
                        let _ = ctx.write_stdout(&format!(
                            "warn ai-hooks {} run external commands from {}",
                            hooks.all().len(),
                            path.display()
                        ));
                    }
                    Err(err) => {
                        let _ = ctx.write_stdout(&format!("error ai-hooks {err}"));
                    }
                },
            }
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
    let mut needs_project_consistency = false;
    let mut needs_shell_proxy_check = false;
    let mut has_rust = false;
    let mut needs_portability = false;

    for path in paths {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.ends_with(".rs") {
            has_rust = true;
        }
        if text == "Cargo.toml" || text.ends_with("/Cargo.toml") || text == "Cargo.lock" {
            needs_workspace_check = true;
        }
        if text == "Cargo.toml"
            || text.ends_with("/Cargo.toml")
            || text == "README.md"
            || text == "LICENSE"
            || text == "scripts/check-project-consistency.py"
        {
            needs_project_consistency = true;
        }
        // `check.sh` runs this on every change, but nothing here suggested it,
        // so a proxy method added over a `doctor validate` cycle only failed in
        // CI.
        if text == "dsh-builtin/src/lib.rs"
            || text == "dsh-builtin/src/shell_capabilities.rs"
            || text == "scripts/check-shell-proxy-capabilities.py"
        {
            needs_shell_proxy_check = true;
        }
        // Linker tuning has to stay scoped to the target that accepts it, and
        // the workflow is where the macOS side is actually proven.
        if text.starts_with(".cargo/") || text.starts_with(".github/workflows/") {
            needs_portability = true;
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

        // `output-schemas/` is embedded the same way (rust-embed), and
        // `command-output-schema.json` mirrors it for `|:`'s output schemas.
        if text.starts_with("output-schemas/") {
            packages.insert("doge-shell");
        }
        if text == "command-output-schema.json" {
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
    // Every Rust edit is a chance to add a one-armed `#[cfg(target_os = ..)]` or
    // an unported `/proc` read, and neither fails a build on the host that wrote
    // it. The lint scans the whole tree either way, so it costs one line.
    if has_rust || needs_portability {
        add_command(&mut commands, "scripts/check-portability.py");
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
            "scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core",
        );
    }
    if needs_project_consistency {
        add_command(&mut commands, "scripts/check-project-consistency.py");
    }
    if needs_shell_proxy_check {
        add_command(&mut commands, "scripts/check-shell-proxy-capabilities.py");
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
mod tests;
