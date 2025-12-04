use crate::ShellProxy;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) const NAME: &str = "ls";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "List files and directories in the specified path.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the directory to list (defaults to current directory)"
                    }
                },
                "required": [],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, _proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for ls tool: {err}"))?;

    let path_value = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let path = Path::new(path_value);

    if path.is_absolute() {
        return Err("chat: ls tool path must be relative".to_string());
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("chat: ls tool path must not contain `..`".to_string());
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
            "chat: ls tool path `{path_value}` resolves outside current directory (resolved to: {})",
            normalized_abs_path.display()
        ));
    }

    if !normalized_abs_path.exists() {
        return Err(format!("chat: path `{path_value}` does not exist"));
    }

    if !normalized_abs_path.is_dir() {
        return Err(format!("chat: path `{path_value}` is not a directory"));
    }

    let mut entries = fs::read_dir(&normalized_abs_path)
        .map_err(|err| format!("chat: failed to read directory `{path_value}`: {err}"))?
        .filter_map(|res| res.ok())
        .collect::<Vec<_>>();

    entries.sort_by_key(|entry| entry.file_name());

    let mut output = String::new();
    output.push_str(&format!("Directory listing for `{}`:\n", path_value));

    if entries.is_empty() {
        output.push_str("(empty directory)");
    } else {
        for entry in entries {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            let metadata = entry.metadata().ok();

            let type_char = if let Some(meta) = &metadata {
                if meta.is_dir() { "d" } else { "-" }
            } else {
                "?"
            };

            let size = if let Some(meta) = &metadata {
                if meta.is_dir() {
                    "-".to_string()
                } else {
                    format!("{}", meta.len())
                }
            } else {
                "?".to_string()
            };

            output.push_str(&format!("{} {:>8} {}\n", type_char, size, name));
        }
    }

    Ok(output)
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
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static CWD_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            vec![]
        }
        fn export_var(&mut self, _key: &str) -> bool {
            true
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
    }

    #[test]
    fn test_ls_current_dir() {
        let _lock = CWD_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();

        env::set_current_dir(&dir).unwrap();
        let mut proxy = NoopProxy;

        let result = run("{}", &mut proxy).unwrap();
        assert!(result.contains("subdir"));
        assert!(result.contains("test.txt"));
        assert!(result.contains("d        - subdir"));
    }

    #[test]
    fn test_ls_subdir() {
        let _lock = CWD_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("file.txt"), "content").unwrap();

        env::set_current_dir(&dir).unwrap();
        let mut proxy = NoopProxy;

        let result = run(r#"{"path": "subdir"}"#, &mut proxy).unwrap();
        assert!(result.contains("file.txt"));
    }

    #[test]
    fn test_ls_outside_workspace() {
        let mut proxy = NoopProxy;
        let result = run(r#"{"path": ".."}"#, &mut proxy);
        assert!(result.is_err());
    }
}
