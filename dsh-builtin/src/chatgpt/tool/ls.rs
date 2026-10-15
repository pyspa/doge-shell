use crate::safety_policy;
use crate::shell_capabilities::ChatToolHost;
use serde_json::{Value, json};
use std::fs;

pub(crate) const NAME: &str = "ls";

/// Entries listed before the rest are reported as a count.
///
/// A `node_modules` or a build output directory holds thousands of names. With
/// no local limit the shared tool-output cap took the *middle* out, so the
/// model saw a listing that looked complete and was not.
const MAX_ENTRIES: usize = 200;

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
                        "description": "Path to the directory to list (relative to current directory or absolute for skills)"
                    }
                },
                "required": [],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, _proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for ls tool: {err}"))?;

    let path_value = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let normalized_abs_path = super::resolve_tool_path(path_value, _proxy)?;
    if let Some(background) = _proxy.background_task_context()
        && !background.read_authorized(&normalized_abs_path)
    {
        return Err("chat: resolved listing target differs from the admitted operation".into());
    }
    let current_dir = _proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir =
        std::fs::canonicalize(&current_dir).unwrap_or_else(|_| super::normalize_path(&current_dir));
    let restricted_io = _proxy.background_task_context().is_some();

    if !restricted_io && !normalized_abs_path.exists() {
        return Err(format!("chat: path `{path_value}` does not exist"));
    }

    if !restricted_io && !normalized_abs_path.is_dir() {
        return Err(format!("chat: path `{path_value}` is not a directory"));
    }

    super::reject_gitignored_read_path(&normalized_abs_path, &normalized_current_dir, path_value)?;

    if let Some(reason) = super::sensitive_path_reason(&normalized_abs_path)
        && !super::confirm_sensitive_access(
            _proxy,
            "list",
            path_value,
            &normalized_abs_path,
            reason,
        )?
    {
        return Ok("ls cancelled by user.".to_string());
    }

    let directory_entries = if restricted_io {
        crate::agent::files::read_directory(&normalized_abs_path)
    } else {
        fs::read_dir(&normalized_abs_path).map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| {
                    let metadata = entry.metadata().ok().map(|metadata| {
                        crate::agent::files::DirectoryMetadata {
                            is_dir: metadata.is_dir(),
                            size: metadata.len(),
                        }
                    });
                    crate::agent::files::DirectoryEntry {
                        name: entry.file_name(),
                        metadata,
                    }
                })
                .collect()
        })
    }
    .map_err(|err| format!("chat: failed to read directory `{path_value}`: {err}"))?;
    let mut hidden_sensitive = 0usize;
    let mut entries = directory_entries
        .into_iter()
        .filter_map(|entry| {
            let path = normalized_abs_path.join(&entry.name);
            match super::reject_gitignored_read_path(
                &path,
                &normalized_current_dir,
                &entry.name.to_string_lossy(),
            ) {
                Ok(()) if safety_policy::is_sensitive_path(&path) => {
                    hidden_sensitive += 1;
                    None
                }
                Ok(()) => Some(entry),
                Err(_) => None,
            }
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| left.name.cmp(&right.name));

    let mut output = String::new();
    output.push_str(&format!("Directory listing for `{}`:\n", path_value));

    if entries.is_empty() {
        output.push_str("(empty directory)");
    } else {
        let total = entries.len();
        for entry in entries.into_iter().take(MAX_ENTRIES) {
            let name = entry.name.to_string_lossy();
            let metadata = entry.metadata;

            let type_char = if let Some(meta) = &metadata {
                if meta.is_dir { "d" } else { "-" }
            } else {
                "?"
            };

            let size = if let Some(meta) = &metadata {
                if meta.is_dir {
                    "-".to_string()
                } else {
                    format!("{}", meta.size)
                }
            } else {
                "?".to_string()
            };

            output.push_str(&format!("{} {:>8} {}\n", type_char, size, name));
        }
        if total > MAX_ENTRIES {
            output.push_str(&format!(
                "... (+{} more entries; use `search` with a glob to narrow this)\n",
                total - MAX_ENTRIES
            ));
        }
        if hidden_sensitive > 0 {
            output.push_str(&format!(
                "... ({hidden_sensitive} sensitive entries hidden)\n"
            ));
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type NoopProxy = TestShellProxy;

    #[test]
    fn test_ls_current_dir() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();

        // env::set_current_dir(&dir).unwrap(); // REMOVED
        let mut proxy = proxy(dir.path().to_path_buf());

        let result = run("{}", &mut proxy).unwrap();
        assert!(result.contains("subdir"));
        assert!(result.contains("test.txt"));
        assert!(result.contains("d        - subdir"));
    }

    #[test]
    fn test_ls_subdir() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("file.txt"), "content").unwrap();

        // env::set_current_dir(&dir).unwrap(); // REMOVED
        let mut proxy = proxy(dir.path().to_path_buf());

        let result = run(r#"{"path": "subdir"}"#, &mut proxy).unwrap();
        assert!(result.contains("file.txt"));
    }

    /// A directory outside the enclosing project stays out of reach.
    ///
    /// This used to assert that `..` is always refused, which stopped being
    /// true once a workspace member gained access to its workspace. The rule
    /// it was really guarding - "nothing beyond the project" - is unchanged, so
    /// the test now builds a parent that is not part of any project.
    #[test]
    fn test_ls_outside_workspace() {
        let outside = tempdir().unwrap();
        let project = outside.path().join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(outside.path().join("secret.txt"), "no").unwrap();

        let mut proxy = proxy(project);
        let result = run(r#"{"path": ".."}"#, &mut proxy);

        assert!(result.is_err(), "{result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn test_ls_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(base.path().join("inside")).unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();
        let mut proxy = proxy(base.path().to_path_buf());

        let result = run(r#"{"path":"inside/link_out"}"#, &mut proxy);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    #[test]
    fn test_ls_hides_gitignored_entries_and_rejects_ignored_directory() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "hidden.txt\nsecret/\n").unwrap();
        fs::write(dir.path().join("visible.txt"), "ok").unwrap();
        fs::write(dir.path().join("hidden.txt"), "secret").unwrap();
        fs::create_dir(dir.path().join("secret")).unwrap();
        fs::write(dir.path().join("secret/file.txt"), "secret").unwrap();
        let mut proxy = proxy(dir.path().to_path_buf());

        let result = run("{}", &mut proxy).unwrap();
        assert!(result.contains("visible.txt"));
        assert!(!result.contains("hidden.txt"));
        assert!(!result.contains("secret"));

        let result = run(r#"{"path":"secret"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ignored by .gitignore"));
    }

    fn proxy(cwd: PathBuf) -> NoopProxy {
        NoopProxy {
            current_dir: cwd,
            confirm_result: true,
            ..NoopProxy::default()
        }
    }

    #[test]
    fn test_ls_hides_sensitive_entries() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("visible.txt"), "ok").unwrap();
        fs::write(dir.path().join(".env.local"), "API_KEY=secret").unwrap();
        let mut proxy = proxy(dir.path().to_path_buf());

        let result = run("{}", &mut proxy).unwrap();

        assert!(result.contains("visible.txt"));
        assert!(!result.contains(".env.local"));
        assert!(result.contains("sensitive entries hidden"));
    }
}
