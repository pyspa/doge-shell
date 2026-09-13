//! `doctor safety`: execute allowlist, MCP transport, AI base URL, envrc,
//! project-skill trust, AI chat hooks, and unignored `*.log` posture.
use crate::ShellProxy;
use crate::project_context;
use crate::safety_policy;
use dsh_types::Context;
use dsh_types::mcp::McpTransport;
use std::path::Path;
use std::process::Command;

use super::*;
pub(super) fn check_safety(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
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

pub(super) fn is_sensitive_env_name(key: &str) -> bool {
    safety_policy::is_sensitive_key(key)
}

pub(super) fn is_https_or_local_http_url(value: &str) -> bool {
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

pub(super) fn is_risky_execute_allowlist_entry(entry: &str) -> bool {
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

pub(super) fn unignored_log_paths(repo_root: &Path) -> std::result::Result<Vec<PathBuf>, String> {
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
