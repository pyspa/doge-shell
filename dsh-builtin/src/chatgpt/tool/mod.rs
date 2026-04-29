use serde_json::Value;
use std::path::{Path, PathBuf};

use super::mcp::McpManager;
use crate::ShellProxy;

mod edit;
mod execute;
mod gitignore;
mod ls;
mod read;
mod search;

const MAX_OUTPUT_LENGTH: usize = 4096;

pub fn build_tools() -> Vec<Value> {
    vec![
        edit::definition(),
        execute::definition(),
        ls::definition(),
        read::definition(),
        search::definition(),
    ]
}

pub fn execute_tool_call(
    tool_call: &Value,
    mcp: &McpManager,
    proxy: &mut dyn ShellProxy,
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
    eprintln!(
        "\x1b[36m🔧 [Tool] {} ({})\x1b[0m",
        name,
        truncate_args(arguments)
    );

    let result = if mcp.has_tool_binding(name) {
        let confirm_msg = format!("AI wants to call MCP tool: `{name}`. \r\nProceed?");
        if !proxy
            .confirm_action(&confirm_msg)
            .map_err(|e: anyhow::Error| e.to_string())?
        {
            return Ok("MCP tool execution cancelled by user.".to_string());
        }

        mcp.execute_tool(name, arguments)?
            .ok_or_else(|| format!("chat: MCP tool binding `{name}` disappeared"))?
    } else {
        match name {
            edit::NAME => edit::run(arguments, proxy)?,
            execute::NAME => execute::run(arguments, proxy)?,
            ls::NAME => ls::run(arguments, proxy)?,
            read::NAME => read::run(arguments, proxy)?,
            search::NAME => search::run(arguments, proxy)?,
            other => return Err(format!("chat: unsupported tool `{other}`")),
        }
    };

    Ok(truncate_output(result))
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

fn truncate_output(output: String) -> String {
    if output.len() > MAX_OUTPUT_LENGTH {
        let end = output.floor_char_boundary(MAX_OUTPUT_LENGTH);
        let truncated = &output[..end];
        let omitted = output.len() - end;
        format!("{truncated}\n... (truncated {omitted} characters)")
    } else {
        output
    }
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
    xdg::BaseDirectories::with_prefix("dsh")
        .ok()
        .map(|dirs| dirs.get_config_home().join("skills"))
        .unwrap_or_else(|| PathBuf::from(".config/dsh/skills"))
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

fn allowed_tool_roots(current_dir: &Path) -> Vec<PathBuf> {
    vec![
        canonicalize_or_normalize(current_dir),
        canonicalize_or_normalize(&tool_skills_dir()),
    ]
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

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::Context;
    use tempfile::tempdir;

    struct NoopProxy;
    impl ShellProxy for NoopProxy {
        fn get_current_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            Ok(std::env::current_dir()?)
        }
        fn exit_shell(&mut self) {}
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
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn unset_env_var(&mut self, _key: &str) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
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
        fn list_mcp_servers(&mut self) -> Vec<dsh_types::mcp::McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            vec![]
        }
        fn export_var(&mut self, _key: &str) -> bool {
            true
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}

        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }

        fn get_git_branch(&self) -> Option<String> {
            None
        }

        fn get_job_count(&self) -> usize {
            0
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
        fn confirm_action(&mut self, _message: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
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
    fn test_truncation_long() {
        let long = "a".repeat(MAX_OUTPUT_LENGTH + 10);
        let truncated = truncate_output(long);
        assert!(truncated.contains("... (truncated 10 characters)"));
        assert_eq!(truncated.len(), MAX_OUTPUT_LENGTH + 30); // 30 is length of "\n... (truncated 10 characters)" approx
    }

    #[test]
    fn test_truncation_no_panic_multi_byte() {
        let mut s = "a".repeat(MAX_OUTPUT_LENGTH - 1);
        s.push('🦀'); // '🦀' is 4 bytes. 4095 + 4 = 4099 bytes.
        // Index 4096 is inside '🦀', floor_char_boundary(4096) should return 4095.
        let truncated = truncate_output(s);
        assert_eq!(truncated.len(), (MAX_OUTPUT_LENGTH - 1) + 29); // 29 is length of "\n... (truncated 4 characters)"
        assert!(truncated.contains("... (truncated 4 characters)"));
    }

    #[test]
    fn test_execute_tool_call_unknown_tool() {
        let mut proxy = NoopProxy;
        let mcp = McpManager::load_blocking(vec![]);
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

    #[test]
    fn execute_tool_call_requires_confirmation_for_mcp_tool() {
        let mut proxy = NoopProxy;
        let mut mcp = McpManager::default();
        mcp.insert_test_tool_binding("mcp__test__tool");
        let tool_call = serde_json::json!({
            "function": {
                "name": "mcp__test__tool",
                "arguments": "{}"
            }
        });

        let result = execute_tool_call(&tool_call, &mcp, &mut proxy).unwrap();

        assert_eq!(result, "MCP tool execution cancelled by user.");
    }

    struct CwdProxy {
        cwd: PathBuf,
    }

    impl ShellProxy for CwdProxy {
        fn get_current_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            Ok(self.cwd.clone())
        }
        fn exit_shell(&mut self) {}
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
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn unset_env_var(&mut self, _key: &str) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
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
        fn list_mcp_servers(&mut self) -> Vec<dsh_types::mcp::McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            vec![]
        }
        fn export_var(&mut self, _key: &str) -> bool {
            true
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }
        fn get_git_branch(&self) -> Option<String> {
            None
        }
        fn get_job_count(&self) -> usize {
            0
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn resolve_tool_path_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = CwdProxy {
            cwd: dir.path().to_path_buf(),
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
            cwd: base.path().to_path_buf(),
        };
        let result = resolve_tool_path("inside/link_out/pwned.txt", &mut proxy);
        assert!(result.is_err());
    }
}
