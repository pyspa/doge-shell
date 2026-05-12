use crate::ShellProxy;
use crate::project_context;
use crate::task;
use dsh_types::{Context, ExitStatus};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn description() -> &'static str {
    "Diagnose config, AI, MCP, project, runtime, skills, setup, and dev validation state"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let section = argv.get(1).map(|value| value.as_str());
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
    if show_section(section, "dev") || show_section(section, "validate") {
        print_header(ctx, "dev");
        check_dev(ctx, &current_dir);
    }

    ExitStatus::ExitedWith(0)
}

fn print_help(ctx: &Context) -> ExitStatus {
    let _ = ctx.write_stdout(help_text());
    ExitStatus::ExitedWith(0)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: doctor [config|ai|mcp|project|runtime|performance|skills|setup|fix|dev|validate] [OPTIONS]\n",
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
        "  setup    Show first-run setup state and recommended next steps\n",
        "  fix      Create safe missing setup directories/files, then show setup state\n",
        "  dev      Suggest validation commands from changed files\n",
        "  validate Alias for dev\n",
        "\n",
        "Examples:\n",
        "  doctor\n",
        "  doctor ai\n",
        "  doctor project\n",
        "  doctor performance --latency --latency-iters 1000\n",
        "  doctor skills\n",
        "  doctor setup\n",
        "  doctor fix\n",
        "  doctor validate\n",
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
    let configured = proxy.list_mcp_servers().len();
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
            for line in lines {
                let _ = ctx.write_stdout(&format!("ok {line}"));
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

            if let Some(slowest) = timing.top_slowest(1).first() {
                let _ = ctx.write_stdout(&format!(
                    "ok slowest {} avg={} success={:.1}%",
                    slowest.command,
                    crate::command_timing::format_duration(slowest.average_duration_ms()),
                    slowest.success_rate()
                ));
            } else {
                let _ = ctx.write_stdout("skip slowest none");
            }

            if let Some(frequent) = timing.top_frequent(1).first() {
                let _ = ctx.write_stdout(&format!(
                    "ok frequent {} calls={}",
                    frequent.command, frequent.total_calls
                ));
            } else {
                let _ = ctx.write_stdout("skip frequent none");
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

fn executable_cache_file_info() -> Option<(PathBuf, usize)> {
    let dirs = xdg::BaseDirectories::with_prefix("dsh").ok()?;
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
            || text.starts_with("docs/ai/")
            || text == "scripts/install-runtime-skills.sh"
        {
            needs_ai_guidance = true;
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
    fn show_section_matches_new_aliases() {
        assert!(show_section(Some("validate"), "dev"));
        assert!(is_known_section("skills"));
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
}
