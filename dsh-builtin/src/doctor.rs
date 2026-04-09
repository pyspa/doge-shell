use crate::ShellProxy;
use crate::project_context;
use dsh_types::{Context, ExitStatus};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn description() -> &'static str {
    "Diagnose config, AI, MCP, project, and runtime state"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let section = argv.get(1).map(|value| value.as_str());
    if matches!(section, Some("-h" | "--help" | "help")) {
        return print_help(ctx);
    }

    let current_dir = proxy
        .get_current_dir()
        .unwrap_or_else(|_| PathBuf::from("."));

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
        check_project(ctx, &current_dir);
    }
    if show_section(section, "runtime") || show_section(section, "runtimes") {
        print_header(ctx, "runtimes");
        check_runtimes(ctx);
    }

    ExitStatus::ExitedWith(0)
}

fn print_help(ctx: &Context) -> ExitStatus {
    let _ = ctx.write_stdout(help_text());
    ExitStatus::ExitedWith(0)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: doctor [config|ai|mcp|project|runtime]\n",
        "\n",
        "Run diagnostics for the current shell setup. Without a section, all checks run.\n",
        "\n",
        "Sections:\n",
        "  config   Check config.lisp and runtime skills directory\n",
        "  ai       Check AI-related environment and defaults\n",
        "  mcp      Check configured MCP servers and connection counters\n",
        "  project  Detect project marker files in the current directory\n",
        "  runtime  Check common developer tools in PATH\n",
        "\n",
        "Examples:\n",
        "  doctor\n",
        "  doctor ai\n",
        "  doctor project\n",
        "  doctor --help\n",
    )
}

fn show_section(selected: Option<&str>, current: &str) -> bool {
    match selected {
        None => true,
        Some("runtime") if current == "runtimes" => true,
        Some("runtimes") if current == "runtime" => true,
        Some(value) => value == current,
    }
}

fn print_header(ctx: &Context, title: &str) {
    let _ = ctx.write_stdout(&format!("[{title}]"));
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

fn check_project(ctx: &Context, current_dir: &Path) {
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
        assert!(help.contains("doctor ai"));
    }

    #[test]
    fn project_section_uses_resolved_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mise.toml"), "[tools]\nnode = '20.11.0'\n").unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();

        let project = project_context::resolve_project_context(dir.path());
        assert_eq!(project.project_root, dir.path());
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
}
