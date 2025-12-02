use crate::ShellProxy;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) const NAME: &str = "read_file";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Read the contents of a file in the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to read (no absolute paths or ..)"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, _proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for read_file tool: {err}"))?;

    let path_value = parsed
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: read_file tool requires `path`".to_string())?;

    if path_value.trim().is_empty() {
        return Err("chat: read_file tool path must not be empty".to_string());
    }

    let path = Path::new(path_value);

    if path.is_absolute() {
        return Err("chat: read_file tool path must be relative".to_string());
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("chat: read_file tool path must not contain `..`".to_string());
    }

    // Get current working directory
    let current_dir = env::current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    // Convert the relative path to an absolute path by joining with current directory
    let abs_path = current_dir.join(path);

    // Normalize the absolute path to resolve any relative components like "." or ".."
    let normalized_abs_path = normalize_path(&abs_path);
    let normalized_current_dir = normalize_path(&current_dir);

    // Check if the resolved path is within the current directory
    if !normalized_abs_path.starts_with(&normalized_current_dir) {
        return Err(format!(
            "chat: read_file tool path `{path_value}` resolves outside current directory (resolved to: {})",
            normalized_abs_path.display()
        ));
    }

    let contents = fs::read_to_string(&normalized_abs_path)
        .map_err(|err| format!("chat: failed to read file `{path_value}`: {err}"))?;

    Ok(contents)
}

// Helper function to normalize a path by resolving all relative components
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::Context;
    use tempfile::tempdir;

    struct NoopProxy;
    impl ShellProxy for NoopProxy {
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
    }

    #[test]
    fn test_read_file_success() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "Hello, world!").unwrap();

        let _guard = env::set_current_dir(&dir).unwrap();
        let mut proxy = NoopProxy;

        let result = run(r#"{"path": "test.txt"}"#, &mut proxy).unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_read_file_not_found() {
        let dir = tempdir().unwrap();
        let _guard = env::set_current_dir(&dir).unwrap();
        let mut proxy = NoopProxy;

        let result = run(r#"{"path": "missing.txt"}"#, &mut proxy);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_file_absolute_path() {
        let mut proxy = NoopProxy;
        let result = run(r#"{"path": "/etc/passwd"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be relative"));
    }

    #[test]
    fn test_read_file_parent_traversal() {
        let mut proxy = NoopProxy;
        let result = run(r#"{"path": "../secret.txt"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not contain `..`"));
    }
}
