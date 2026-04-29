use crate::ShellProxy;
use anyhow::Result;
use serde_json::{Value, json};
use std::fs;

pub(crate) const NAME: &str = "edit";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Create or overwrite a workspace file with the provided contents.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to current directory or absolute for skills)"
                    },
                    "contents": {
                        "type": "string",
                        "description": "Full desired contents of the file. The file will be overwritten with this value."
                    }
                },
                "required": ["path", "contents"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for edit tool: {err}"))?;

    let path_value = parsed
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: edit tool requires `path`".to_string())?;

    if path_value.trim().is_empty() {
        return Err("chat: edit tool path must not be empty".to_string());
    }

    let contents = parsed
        .get("contents")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: edit tool requires `contents`".to_string())?;

    let normalized_abs_path = super::resolve_tool_path(path_value, proxy)?;

    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir =
        std::fs::canonicalize(&current_dir).unwrap_or_else(|_| super::normalize_path(&current_dir));

    if super::gitignore::is_gitignored(&normalized_abs_path, &normalized_current_dir) {
        return Err(format!(
            "chat: edit tool path `{path_value}` is ignored by .gitignore"
        ));
    }

    if let Some(parent) = normalized_abs_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("chat: failed to create parent directories: {err}"))?;
    }

    // Safety Guard: Request confirmation from user
    let confirm_msg = format!("AI wants to write to file: `{}`. \r\nProceed?", path_value);
    if !proxy
        .confirm_action(&confirm_msg)
        .map_err(|e: anyhow::Error| e.to_string())?
    {
        return Ok("File modification cancelled by user.".to_string());
    }

    fs::write(&normalized_abs_path, contents)
        .map_err(|err| format!("chat: failed to write file `{path_value}`: {err}"))?;

    Ok(format!(
        "edit completed: wrote {} bytes to {}",
        contents.len(),
        path_value
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::Context;
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::tempdir;

    struct TestProxy {
        cwd: PathBuf,
        confirm_calls: Arc<AtomicUsize>,
        confirm_result: bool,
    }

    impl ShellProxy for TestProxy {
        fn get_current_dir(&self) -> anyhow::Result<PathBuf> {
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
        fn confirm_action(&mut self, _message: &str) -> anyhow::Result<bool> {
            self.confirm_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.confirm_result)
        }
    }

    fn proxy(cwd: PathBuf) -> TestProxy {
        TestProxy {
            cwd,
            confirm_calls: Arc::new(AtomicUsize::new(0)),
            confirm_result: true,
        }
    }

    #[test]
    fn edit_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path().to_path_buf());

        let result = run(r#"{"path":"../outside.txt","contents":"x"}"#, &mut proxy);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    #[cfg(unix)]
    #[test]
    fn edit_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(base.path().join("inside")).unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();
        let mut proxy = proxy(base.path().to_path_buf());

        let result = run(
            r#"{"path":"inside/link_out/pwned.txt","contents":"x"}"#,
            &mut proxy,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    #[test]
    fn edit_rejects_gitignored_path_before_confirmation() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "secrets/\n").unwrap();
        fs::create_dir(dir.path().join("secrets")).unwrap();
        let confirm_calls = Arc::new(AtomicUsize::new(0));
        let mut proxy = TestProxy {
            cwd: dir.path().to_path_buf(),
            confirm_calls: confirm_calls.clone(),
            confirm_result: true,
        };

        let result = run(
            r#"{"path":"secrets/out.txt","contents":"secret"}"#,
            &mut proxy,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ignored by .gitignore"));
        assert_eq!(confirm_calls.load(Ordering::SeqCst), 0);
        assert!(!dir.path().join("secrets/out.txt").exists());
    }
}
