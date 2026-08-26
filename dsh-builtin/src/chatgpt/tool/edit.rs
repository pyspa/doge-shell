use crate::ShellProxy;
use crate::safety_policy;
use anyhow::Result;
use serde_json::{Value, json};
use std::fs;

pub(crate) const NAME: &str = "edit";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Create a new workspace file, or replace an existing one in full. The file is overwritten with `contents`, so use `str_replace` instead when changing part of an existing file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to current directory or absolute for skills)"
                    },
                    "contents": {
                        "type": "string",
                        "description": "Full desired contents of the file. Everything currently in the file is discarded."
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

    super::reject_gitignored_path(&normalized_abs_path, &normalized_current_dir, path_value)?;

    if let Some(parent) = normalized_abs_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("chat: failed to create parent directories: {err}"))?;
    }

    // Safety Guard: Request confirmation from user
    let sensitive_note = if safety_policy::is_sensitive_path(&normalized_abs_path)
        || safety_policy::contains_sensitive_text(contents)
    {
        " Sensitive path or content detected."
    } else {
        ""
    };
    let confirm_msg = format!(
        "AI wants to write to file: `{}`.{} \r\nProceed?",
        path_value, sensitive_note
    );
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
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type TestProxy = TestShellProxy;

    fn proxy(cwd: PathBuf) -> TestProxy {
        TestProxy {
            current_dir: cwd,
            confirm_result: true,
            ..TestProxy::default()
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
            current_dir: dir.path().to_path_buf(),
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: true,
            ..TestProxy::default()
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

    #[test]
    fn edit_mentions_sensitive_content_in_confirmation() {
        let dir = tempdir().unwrap();
        let confirm_calls = Arc::new(AtomicUsize::new(0));
        let mut proxy = TestProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_counter: Some(confirm_calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let result = run(
            r#"{"path":"config.txt","contents":"API_KEY=secret"}"#,
            &mut proxy,
        )
        .unwrap();

        assert_eq!(result, "File modification cancelled by user.");
        assert_eq!(confirm_calls.load(Ordering::SeqCst), 1);
        assert!(!dir.path().join("config.txt").exists());
    }
}
