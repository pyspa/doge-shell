use parking_lot::RwLock;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::mcp::McpManager;
use crate::ShellProxy;
use crate::safety_policy::{self, SafetyLevel};
use crate::shell_capabilities::{AgentCommandVerdict, ApprovalDecision, ChatToolHost};

mod edit;
mod execute;
mod gitignore;
mod ls;
mod read;
mod replace;
mod search;
mod shell_context;
mod shell_history;

/// Global backstop for the size of a single tool result. Individual tools apply
/// their own tighter limits first so that the important part of their output
/// survives this cut.
///
/// Shared with the shell-side loop, which used half this and so fed the same
/// tool a different amount of room depending on the entry point.
pub(crate) const MAX_OUTPUT_LENGTH: usize = dsh_openai::turn::limits::MAX_TOOL_OUTPUT_CHARS;

pub fn build_tools() -> Vec<Value> {
    vec![
        edit::definition(),
        execute::definition(),
        ls::definition(),
        read::definition(),
        replace::definition(),
        search::definition(),
        shell_context::definition(),
        shell_history::definition(),
    ]
}

pub fn execute_tool_call(
    tool_call: &Value,
    mcp: &Arc<RwLock<McpManager>>,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    let function = tool_call
        .get("function")
        .ok_or_else(|| "chat: tool call missing function".to_string())?;

    let name = function
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: tool call missing function name".to_string())?;

    let arguments = function
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    // Log tool execution
    let logged_arguments = redact_tool_arguments(arguments);
    eprintln!(
        "\x1b[36m🔧 [Tool] {} ({})\x1b[0m",
        name,
        truncate_args(&logged_arguments)
    );

    // Take the lock for each short call rather than holding it across the
    // confirmation prompt: the user can sit on that question for a long time,
    // and `mcp connect` in another turn needs the write lock.
    let is_mcp_tool = mcp.read().has_tool_binding(name);
    let result = if is_mcp_tool {
        if !authorize_mcp_tool(name, arguments, proxy)? {
            return Ok("MCP tool execution cancelled by user.".to_string());
        }

        let executed = mcp.read().execute_tool(name, arguments)?;
        executed.ok_or_else(|| format!("chat: MCP tool binding `{name}` disappeared"))?
    } else {
        match name {
            edit::NAME => edit::run(arguments, proxy)?,
            execute::NAME => execute::run(arguments, proxy)?,
            ls::NAME => ls::run(arguments, proxy)?,
            read::NAME => read::run(arguments, proxy)?,
            replace::NAME => replace::run(arguments, proxy)?,
            search::NAME => search::run(arguments, proxy)?,
            shell_context::NAME => shell_context::run(arguments, proxy)?,
            shell_history::NAME => shell_history::run(arguments, proxy)?,
            other => return Err(format!("chat: unsupported tool `{other}`")),
        }
    };

    Ok(truncate_output(result))
}

/// Put an MCP call through the shell's own safety policy.
///
/// This used to prompt unconditionally, which meant `loose` still asked about a
/// read-only tool and "always" was unreachable - the opposite of what the same
/// call got through the shell-side AI service.
fn authorize_mcp_tool(
    name: &str,
    arguments: &str,
    proxy: &mut dyn ChatToolHost,
) -> Result<bool, String> {
    match proxy.evaluate_agent_tool(name, arguments) {
        AgentCommandVerdict::Allowed => Ok(true),
        AgentCommandVerdict::Denied(reason) => {
            Err(format!("chat: MCP tool `{name}` refused: {reason}"))
        }
        AgentCommandVerdict::Confirm(reason) => {
            let message = format!("AI wants to call MCP tool: `{name}` ({reason}). \r\nProceed?");
            match proxy
                .request_agent_approval(&message)
                .map_err(|err: anyhow::Error| err.to_string())?
            {
                ApprovalDecision::Allow => Ok(true),
                ApprovalDecision::AllowAlways => {
                    let entry = proxy.agent_tool_approval_entry(name, arguments);
                    proxy.remember_agent_approval(&entry);
                    Ok(true)
                }
                ApprovalDecision::Deny => Ok(false),
            }
        }
    }
}

fn truncate_args(args: &str) -> String {
    const MAX_ARGS_LEN: usize = 80;
    if args.len() > MAX_ARGS_LEN {
        let end = args.floor_char_boundary(MAX_ARGS_LEN);
        format!("{}...", &args[..end])
    } else {
        args.to_string()
    }
}

fn redact_tool_arguments(args: &str) -> String {
    safety_policy::redact_sensitive_text(args)
}

fn truncate_output(output: String) -> String {
    if output.len() <= MAX_OUTPUT_LENGTH {
        return output;
    }
    dsh_openai::turn::truncate_middle(&output, MAX_OUTPUT_LENGTH)
}

pub(crate) fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {
                // Skip current directory components
            }
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}

pub(crate) fn tool_skills_dir() -> PathBuf {
    crate::config_paths::skills_dir()
}

fn canonicalize_or_normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

fn resolve_with_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut current = path.to_path_buf();
    let mut suffix = PathBuf::new();

    loop {
        if current.exists() {
            let canonical = std::fs::canonicalize(&current).map_err(|err| {
                format!(
                    "chat: failed to canonicalize path ancestor `{}`: {err}",
                    current.display()
                )
            })?;
            return Ok(if suffix.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(suffix)
            });
        }

        let name = current.file_name().ok_or_else(|| {
            format!(
                "chat: path `{}` has no existing ancestor",
                path.to_string_lossy()
            )
        })?;
        suffix = PathBuf::from(name).join(suffix);

        if !current.pop() {
            return Err(format!(
                "chat: path `{}` has no existing ancestor",
                path.to_string_lossy()
            ));
        }
    }
}

/// The directories a tool may touch.
///
/// The project root, not just the current directory: in a workspace, running
/// `!` from `dsh-builtin/` used to put the top-level `Cargo.toml` and every
/// sibling crate out of reach, with an error message that offered no way
/// around it. The root is the nearest ancestor carrying a project marker, so
/// this widens to the repository and stops there rather than at `$HOME`.
fn allowed_tool_roots(current_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![canonicalize_or_normalize(current_dir)];
    let workspace_root = canonicalize_or_normalize(&workspace_root(current_dir));
    if !roots.contains(&workspace_root) {
        roots.push(workspace_root);
    }
    roots.push(canonicalize_or_normalize(&tool_skills_dir()));
    roots
}

/// The outermost project enclosing `current_dir`.
///
/// `find_project_root` stops at the *nearest* marker, which for a workspace
/// member is the member itself - so running `!` in `dsh-builtin/` still could
/// not see the workspace `Cargo.toml` one level up. Walking while each parent
/// is also a project extends the reach to the repository and stops there: the
/// chain breaks at the first directory that is not a project, so an unrelated
/// parent never becomes readable.
///
/// Never expands to the home directory itself, which a dotfiles repository
/// would otherwise qualify by way of its `.git`.
fn workspace_root(current_dir: &Path) -> PathBuf {
    let current_dir = canonicalize_or_normalize(current_dir);
    let home = dirs::home_dir().map(|home| canonicalize_or_normalize(&home));
    let too_far = |candidate: &Path| home.as_deref().is_some_and(|home| candidate == home);

    let mut root =
        canonicalize_or_normalize(&crate::project_context::find_project_root(&current_dir));

    // `find_project_root` walks ancestors, so with a dotfiles repository in
    // `$HOME` it answers `$HOME` for any directory that is not itself a
    // project - which would have put `~/.ssh` and `~/.aws` inside the sandbox.
    // Checking the starting point, not only the climb, is what stops that.
    if too_far(&root) {
        return current_dir;
    }

    while let Some(parent) = root.parent() {
        if too_far(parent) || !crate::project_context::has_project_marker(parent) {
            break;
        }
        root = parent.to_path_buf();
    }

    root
}

pub(crate) fn is_path_within_tool_roots(path: &Path, current_dir: &Path) -> bool {
    let roots = allowed_tool_roots(current_dir);
    roots.iter().any(|root| path.starts_with(root))
}

pub(crate) fn resolve_tool_path(
    path_str: &str,
    proxy: &mut dyn ShellProxy,
) -> Result<std::path::PathBuf, String> {
    // Use shellexpand to handle ~
    let expanded = shellexpand::full(path_str)
        .map_err(|e| format!("chat: failed to expand path `{path_str}`: {e}"))?;
    let path = Path::new(expanded.as_ref());
    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };
    let resolved_path = if absolute_path.exists() {
        std::fs::canonicalize(&absolute_path).map_err(|err| {
            format!(
                "chat: failed to canonicalize path `{}`: {err}",
                absolute_path.display()
            )
        })?
    } else {
        resolve_with_existing_ancestor(&absolute_path)?
    };

    if is_path_within_tool_roots(&resolved_path, &current_dir) {
        return Ok(resolved_path);
    }

    Err(format!(
        "chat: path `{path_str}` resolves outside allowed directories"
    ))
}

pub(crate) fn safety_level(proxy: &mut dyn ShellProxy) -> SafetyLevel {
    proxy.safety_level()
}

pub(crate) fn reject_gitignored_path(
    path: &Path,
    base_dir: &Path,
    user_path: &str,
) -> Result<(), String> {
    match gitignore::is_gitignored(path, base_dir) {
        Ok(false) => Ok(()),
        Ok(true) => Err(format!(
            "chat: tool path `{user_path}` is ignored by .gitignore"
        )),
        Err(err) => Err(format!("chat: failed to apply .gitignore policy: {err}")),
    }
}

pub(crate) fn confirm_sensitive_access(
    proxy: &mut dyn ShellProxy,
    action: &str,
    path_label: &str,
    reason: &str,
) -> Result<bool, String> {
    if !safety_level(proxy).requires_confirmation_for_sensitive_access() {
        return Ok(true);
    }

    let message =
        format!("AI wants to {action} sensitive content `{path_label}` ({reason}). \r\nProceed?");
    proxy
        .confirm_action(&message)
        .map_err(|err| format!("chat: confirmation failed: {err}"))
}

pub(crate) fn sensitive_path_reason(path: &Path) -> Option<&'static str> {
    safety_policy::is_sensitive_path(path).then_some("sensitive path")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type NoopProxy = TestShellProxy;

    /// A workspace member must be able to see the workspace.
    #[test]
    fn tool_roots_reach_the_project_root_from_a_subdirectory() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let member = root.join("crates/inner");
        std::fs::create_dir_all(&member).unwrap();

        assert!(is_path_within_tool_roots(&root.join("Cargo.toml"), &member));
    }

    /// The member carries its own `Cargo.toml`, so stopping at the nearest
    /// marker left the workspace file one level up out of reach.
    #[test]
    fn tool_roots_climb_past_a_member_that_is_itself_a_project() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let member = root.join("member");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\n").unwrap();

        assert!(is_path_within_tool_roots(&root.join("Cargo.toml"), &member));
    }

    /// A dotfiles repository in `$HOME` makes `find_project_root` answer
    /// `$HOME` for any plain directory beneath it. Widening to that answer put
    /// `~/.ssh` and `~/.aws` inside the sandbox of a shell started in, say,
    /// `~/scratch`.
    ///
    /// The working directory itself is a root either way - that is the original
    /// contract, and running `!` from `$HOME` has always meant that much. What
    /// must not happen is *reaching* `$HOME` from somewhere below it.
    #[test]
    fn widening_never_climbs_out_into_the_home_directory() {
        let _lock = execute::tests::env_lock();
        let fake_home = tempdir().unwrap();
        let home = std::fs::canonicalize(fake_home.path()).unwrap();
        std::fs::create_dir(home.join(".git")).unwrap();

        let scratch = home.join("scratch");
        std::fs::create_dir(&scratch).unwrap();

        // SAFETY: single-threaded under the shared env lock.
        let previous = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };
        let root = workspace_root(&scratch);
        let reaches_home = is_path_within_tool_roots(&home.join(".ssh"), &scratch);
        match previous {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }

        assert_eq!(root, scratch, "widening must stop below $HOME");
        assert!(!reaches_home, "$HOME must stay outside the sandbox");
    }

    /// Widening stops at the project, not at the home directory.
    #[test]
    fn tool_roots_do_not_reach_above_the_project() {
        let dir = tempdir().unwrap();
        let outside = std::fs::canonicalize(dir.path()).unwrap();
        let root = outside.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(outside.join("secrets.txt"), "no").unwrap();

        assert!(!is_path_within_tool_roots(
            &outside.join("secrets.txt"),
            &root
        ));
    }

    #[test]
    fn test_truncation_short() {
        let short = "Short output";
        assert_eq!(truncate_output(short.to_string()), short);
    }

    #[test]
    fn test_truncation_exact() {
        let exact = "a".repeat(MAX_OUTPUT_LENGTH);
        assert_eq!(truncate_output(exact.clone()), exact);
    }

    #[test]
    fn truncate_output_keeps_the_tail() {
        // The tail carries the compiler error / test summary the model must see.
        let long = format!("{}{}", "H".repeat(MAX_OUTPUT_LENGTH), "TAIL-MARKER");
        let truncated = truncate_output(long);

        assert!(truncated.starts_with("HHH"));
        assert!(truncated.ends_with("TAIL-MARKER"));
        assert!(truncated.contains("truncated"));
        assert!(truncated.len() < MAX_OUTPUT_LENGTH + 64);
    }

    #[test]
    fn execute_tool_call_returns_parseable_json_after_the_global_cap() {
        // The global cap runs after the tool, so it must not corrupt a
        // structured result on its way back to the model.
        //
        // `EXECUTE_TOOL_ENV_ALLOWLIST` is process-global and wins over the
        // proxy's list, so this has to hold the same lock as the tests that set
        // it or `ls` stops being allowed halfway through the run.
        let _lock = super::execute::tests::ENV_LOCK.lock().unwrap();
        let _env_guard =
            super::execute::tests::EnvGuard::set(super::execute::EXECUTE_TOOL_ENV_ALLOWLIST, "ls");

        let dir = tempdir().unwrap();
        for index in 0..400 {
            std::fs::write(dir.path().join(format!("f-{index:0>50}")), b"x").unwrap();
        }

        let mut proxy = NoopProxy {
            current_dir: std::env::current_dir().unwrap(),
            execute_allowlist: vec!["ls".to_string()],
            confirm_result: true,
            ..NoopProxy::default()
        };

        let tool_call = json!({
            "function": {
                "name": "execute",
                "arguments": format!("{{\"command\":\"ls -R {}\"}}", dir.path().display())
            }
        });

        let result = execute_tool_call(
            &tool_call,
            &Arc::new(RwLock::new(crate::chatgpt::McpManager::default())),
            &mut proxy,
        )
        .unwrap();

        assert!(result.len() <= MAX_OUTPUT_LENGTH + 128);
        serde_json::from_str::<Value>(&result).expect("tool result must stay valid JSON");
    }

    #[test]
    fn tool_argument_log_redacts_secret_like_values() {
        let args =
            r#"{"path":"config.txt","contents":"API_KEY=secret Authorization: Bearer token"}"#;
        let redacted = redact_tool_arguments(args);

        assert!(redacted.contains("API_KEY=***"));
        assert!(redacted.contains("Authorization: Bearer ***"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("token"));
    }

    #[test]
    fn test_execute_tool_call_unknown_tool() {
        let mut proxy = NoopProxy::default();
        let mcp = Arc::new(RwLock::new(McpManager::load_blocking(vec![])));
        let tool_call = serde_json::json!({
            "function": {
                "name": "unknown_tool",
                "arguments": "{}"
            }
        });

        let result = execute_tool_call(&tool_call, &mcp, &mut proxy);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "chat: unsupported tool `unknown_tool`");
    }

    /// The policy decides, not the call site. `!` used to prompt for every MCP
    /// call at every safety level, which `loose` was supposed to switch off and
    /// which made "always" unreachable.
    #[test]
    fn an_allowed_mcp_tool_runs_without_asking() {
        let mut proxy = NoopProxy {
            agent_tool_verdict: AgentCommandVerdict::Allowed,
            ..NoopProxy::default()
        };
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {"name": "mcp__test__tool", "arguments": "{}"}
        });

        // No binding is actually connected, so the call fails after the gate -
        // what matters is that the gate did not ask.
        let _ = execute_tool_call(&tool_call, &mcp, &mut proxy);
        assert_eq!(proxy.confirm_calls, 0);
    }

    #[test]
    fn a_denied_mcp_tool_is_refused_without_asking() {
        let mut proxy = NoopProxy {
            agent_tool_verdict: AgentCommandVerdict::Denied("policy says no".to_string()),
            ..NoopProxy::default()
        };
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {"name": "mcp__test__tool", "arguments": "{}"}
        });

        let err = execute_tool_call(&tool_call, &mcp, &mut proxy).unwrap_err();
        assert!(err.contains("policy says no"));
        assert_eq!(proxy.confirm_calls, 0);
    }

    /// "always" was unreachable while this path used the bool `confirm_action`.
    #[test]
    fn an_always_answer_is_remembered_for_the_session() {
        let mut proxy = NoopProxy {
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..NoopProxy::default()
        };
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {"name": "mcp__test__tool", "arguments": "{}"}
        });

        let _ = execute_tool_call(&tool_call, &mcp, &mut proxy);
        assert_eq!(proxy.agent_session_allowlist, vec!["mcp:mcp__test__tool"]);
    }

    #[test]
    fn execute_tool_call_requires_confirmation_for_mcp_tool() {
        let mut proxy = NoopProxy::default();
        let mut inner = McpManager::default();
        inner.insert_test_tool_binding("mcp__test__tool");
        let mcp = Arc::new(RwLock::new(inner));
        let tool_call = serde_json::json!({
            "function": {
                "name": "mcp__test__tool",
                "arguments": "{}"
            }
        });

        let result = execute_tool_call(&tool_call, &mcp, &mut proxy).unwrap();

        assert_eq!(result, "MCP tool execution cancelled by user.");
    }

    type CwdProxy = TestShellProxy;

    #[test]
    fn resolve_tool_path_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = CwdProxy {
            current_dir: dir.path().to_path_buf(),
            ..CwdProxy::default()
        };
        let result = resolve_tool_path("../outside.txt", &mut proxy);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_tool_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("inside")).unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();

        let mut proxy = CwdProxy {
            current_dir: base.path().to_path_buf(),
            ..CwdProxy::default()
        };
        let result = resolve_tool_path("inside/link_out/pwned.txt", &mut proxy);
        assert!(result.is_err());
    }
}
