use crate::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn description() -> &'static str {
    "Diagnose config, AI, MCP, project context, and toolchain availability"
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
    let _ = ctx.write_stdout("doctor [config|ai|mcp|project|runtime]");
    ExitStatus::ExitedWith(0)
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
    let _ = ctx.write_stdout(&format!("ok cwd {}", current_dir.display()));
    for (label, present) in [
        ("Cargo.toml", current_dir.join("Cargo.toml").exists()),
        ("package.json", current_dir.join("package.json").exists()),
        (
            "pyproject.toml",
            current_dir.join("pyproject.toml").exists(),
        ),
        ("mise.toml", current_dir.join("mise.toml").exists()),
        (
            ".tool-versions",
            current_dir.join(".tool-versions").exists(),
        ),
        (
            ".python-version",
            current_dir.join(".python-version").exists(),
        ),
        (".node-version", current_dir.join(".node-version").exists()),
        (".nvmrc", current_dir.join(".nvmrc").exists()),
        ("Justfile", current_dir.join("Justfile").exists()),
        ("Taskfile.yml", current_dir.join("Taskfile.yml").exists()),
        (".envrc", current_dir.join(".envrc").exists()),
        (".env", current_dir.join(".env").exists()),
    ] {
        let state = if present { "ok" } else { "skip" };
        let _ = ctx.write_stdout(&format!("{state} detect {label}"));
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
}
