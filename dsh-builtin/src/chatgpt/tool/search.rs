use crate::ShellProxy;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

pub(crate) const NAME: &str = "search";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Search for files by name or content.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query (filename pattern or content string)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Relative path to start search from (defaults to current directory)"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["filename", "content"],
                        "description": "Type of search: 'filename' (glob pattern) or 'content' (text search)"
                    }
                },
                "required": ["query", "type"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, _proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for search tool: {err}"))?;

    let query = parsed
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: search tool requires `query`".to_string())?;

    let search_type = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: search tool requires `type`".to_string())?;

    let path_value = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let path = Path::new(path_value);

    if path.is_absolute() {
        return Err("chat: search tool path must be relative".to_string());
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("chat: search tool path must not contain `..`".to_string());
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
            "chat: search tool path `{path_value}` resolves outside current directory (resolved to: {})",
            normalized_abs_path.display()
        ));
    }

    if !normalized_abs_path.exists() {
        return Err(format!("chat: path `{path_value}` does not exist"));
    }

    let mut results = Vec::new();
    let max_results = 50;

    match search_type {
        "filename" => {
            let glob_pattern = format!("**/{}", query);
            let glob = glob::Pattern::new(&glob_pattern)
                .map_err(|err| format!("chat: invalid glob pattern: {err}"))?;

            for entry in WalkDir::new(&normalized_abs_path)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }

                if entry.file_type().is_dir() {
                    continue;
                }

                // Check if file matches glob
                // We match against the relative path from the search root
                if let Ok(_rel_path) = entry.path().strip_prefix(&normalized_abs_path) {
                    // For glob matching, we want to match against the filename or path
                    // Simple implementation: check if filename matches
                    if glob.matches_path(entry.path())
                        || glob.matches(entry.file_name().to_str().unwrap_or(""))
                    {
                        // Get path relative to CWD for output
                        if let Ok(cwd_rel) = entry.path().strip_prefix(&current_dir) {
                            results.push(cwd_rel.display().to_string());
                        }
                    }
                }
            }
        }
        "content" => {
            for entry in WalkDir::new(&normalized_abs_path)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }

                if !entry.file_type().is_file() {
                    continue;
                }

                // Skip binary files (heuristic)
                // For now, just try to read as text
                if let Ok(file) = fs::File::open(entry.path()) {
                    let reader = BufReader::new(file);
                    for (line_idx, line) in reader.lines().enumerate() {
                        if let Ok(line_content) = line
                            && line_content.contains(query)
                        {
                            if let Ok(cwd_rel) = entry.path().strip_prefix(&current_dir) {
                                results.push(format!(
                                    "{}:{}: {}",
                                    cwd_rel.display(),
                                    line_idx + 1,
                                    line_content.trim()
                                ));
                            }
                            break; // Only one match per file for now to avoid spam
                        }
                    }
                }
            }
        }
        _ => return Err(format!("chat: unsupported search type `{search_type}`")),
    }

    let mut output = String::new();
    output.push_str(&format!(
        "Search results for `{}` in `{}`:\n",
        query, path_value
    ));

    if results.is_empty() {
        output.push_str("(no matches found)");
    } else {
        for result in results {
            output.push_str(&format!("- {}\n", result));
        }
        if output.lines().count() > max_results {
            output.push_str("... (results truncated)");
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
    fn test_search_filename() {
        let _lock = CWD_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_file.rs");
        fs::write(&file_path, "content").unwrap();

        let _guard = env::set_current_dir(&dir).unwrap();
        let mut proxy = NoopProxy;

        let result = run(
            r#"{"query": "test_file.rs", "type": "filename"}"#,
            &mut proxy,
        )
        .unwrap();
        assert!(result.contains("test_file.rs"));
    }

    #[test]
    fn test_search_content() {
        let _lock = CWD_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let _guard = env::set_current_dir(&dir).unwrap();
        let mut proxy = NoopProxy;

        let result = run(r#"{"query": "world", "type": "content"}"#, &mut proxy).unwrap();
        assert!(result.contains("test.txt"));
        assert!(result.contains("hello world"));
    }
}
