use crate::ShellProxy;
use crate::safety_policy;
use serde_json::{Value, json};
use std::fs;

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
                        "description": "Path to the file to read (relative to current directory or absolute for skills)"
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

    let normalized_abs_path = super::resolve_tool_path(path_value, _proxy)?;

    // Get CWD for gitignore check
    let current_dir = _proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir =
        std::fs::canonicalize(&current_dir).unwrap_or_else(|_| super::normalize_path(&current_dir));

    super::reject_gitignored_path(&normalized_abs_path, &normalized_current_dir, path_value)?;

    if let Some(reason) = super::sensitive_path_reason(&normalized_abs_path)
        && !super::confirm_sensitive_access(_proxy, "read", path_value, reason)?
    {
        return Ok("read_file cancelled by user.".to_string());
    }

    let contents = fs::read_to_string(&normalized_abs_path)
        .map_err(|err| format!("chat: failed to read file `{path_value}`: {err}"))?;

    if safety_policy::contains_sensitive_text(&contents)
        && !super::confirm_sensitive_access(_proxy, "read", path_value, "sensitive content")?
    {
        return Ok("read_file cancelled by user.".to_string());
    }

    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type NoopProxy = TestShellProxy;

    #[test]
    fn test_read_file_success() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "Hello, world!").unwrap();

        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "test.txt"}"#, &mut proxy).unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_read_file_not_found() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "missing.txt"}"#, &mut proxy);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_file_absolute_path() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());
        let result = run(r#"{"path": "/etc/passwd"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("resolves outside allowed directories")
        );
    }

    #[test]
    fn test_read_file_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());
        let result = run(r#"{"path": "../secret.txt"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    #[cfg(unix)]
    #[test]
    fn test_read_file_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(base.path().join("inside")).unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();
        let mut proxy = proxy(base.path());

        let result = run(r#"{"path": "inside/link_out/secret.txt"}"#, &mut proxy);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    fn proxy(path: &std::path::Path) -> NoopProxy {
        NoopProxy {
            current_dir: path.to_path_buf(),
            confirm_result: true,
            ..NoopProxy::default()
        }
    }

    #[test]
    fn test_read_file_requires_confirmation_for_sensitive_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".env.local"), "APP_MODE=dev").unwrap();
        let mut proxy = NoopProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: false,
            ..NoopProxy::default()
        };

        let result = run(r#"{"path": ".env.local"}"#, &mut proxy).unwrap();

        assert_eq!(result, "read_file cancelled by user.");
        assert_eq!(proxy.confirm_calls, 1);
    }

    #[test]
    fn test_read_file_requires_confirmation_for_sensitive_content() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("config.txt"), "API_KEY=secret").unwrap();
        let mut proxy = NoopProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: false,
            ..NoopProxy::default()
        };

        let result = run(r#"{"path": "config.txt"}"#, &mut proxy).unwrap();

        assert_eq!(result, "read_file cancelled by user.");
        assert_eq!(proxy.confirm_calls, 1);
    }
}
