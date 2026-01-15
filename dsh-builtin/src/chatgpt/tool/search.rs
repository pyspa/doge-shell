use crate::ShellProxy;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader};

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
                        "description": "Path to start search from (relative to current directory or absolute for skills)"
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

    let normalized_abs_path = super::resolve_tool_path(path_value, _proxy)?;

    // Get CWD for output stripping
    let current_dir = _proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir = super::normalize_path(&current_dir);

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

            // Use ignore::WalkBuilder to automatically respect .gitignore
            for entry in WalkBuilder::new(&normalized_abs_path)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build()
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }

                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }

                // Check if file matches glob
                // We match against the relative path from the search root
                if let Ok(_rel_path) = entry.path().strip_prefix(&normalized_abs_path) {
                    // For glob matching, we want to match against the filename or path
                    // Simple implementation: check if filename matches
                    if glob.matches_path(entry.path())
                        || entry
                            .file_name()
                            .to_str()
                            .map(|s| glob.matches(s))
                            .unwrap_or(false)
                    {
                        // Get path relative to CWD for output
                        if let Ok(cwd_rel) = entry.path().strip_prefix(&normalized_current_dir) {
                            results.push(cwd_rel.display().to_string());
                        }
                    }
                }
            }
        }
        "content" => {
            // Use ignore::WalkBuilder to automatically respect .gitignore
            for entry in WalkBuilder::new(&normalized_abs_path)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build()
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }

                if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
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
                            if let Ok(cwd_rel) = entry.path().strip_prefix(&normalized_current_dir)
                            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::Context;
    use tempfile::tempdir;

    struct NoopProxy {
        cwd: std::path::PathBuf,
    }
    impl ShellProxy for NoopProxy {
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
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }

        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }

        fn get_git_branch(&self) -> Option<String> {
            None
        }

        fn get_job_count(&self) -> usize {
            0
        }
    }

    #[test]
    fn test_search_filename() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_file.rs");
        fs::write(&file_path, "content").unwrap();

        let mut proxy = NoopProxy {
            cwd: dir.path().to_path_buf(),
        };

        let result = run(
            r#"{"query": "test_file.rs", "type": "filename"}"#,
            &mut proxy,
        )
        .unwrap();
        assert!(result.contains("test_file.rs"));
    }

    #[test]
    fn test_search_content() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let mut proxy = NoopProxy {
            cwd: dir.path().to_path_buf(),
        };

        let result = run(r#"{"query": "world", "type": "content"}"#, &mut proxy).unwrap();
        assert!(result.contains("test.txt"));
        assert!(result.contains("hello world"));
    }
}
